#include "LiliaJsi.h"

#include "LiliaCAbi.h"

#include <algorithm>
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <functional>
#include <mutex>
#include <shared_mutex>
#include <stdexcept>
#include <string>
#include <thread>
#include <utility>
#include <variant>
#include <vector>

namespace liliadb {

namespace jsi = facebook::jsi;
using facebook::react::CallInvoker;

namespace {

/// How long a database nobody closed may take to close when JavaScript drops it.
constexpr uint64_t kDropCloseTimeoutMs = 5000;

/// The threads asynchronous calls run on, shared by every database.
class Workers {
public:
  /// The pool, created on first use. It is never destroyed: a worker may still be running a
  /// call while the process exits, after static destructors would have run.
  static Workers &shared() {
    static Workers *workers = new Workers(std::max(2U, std::thread::hardware_concurrency() / 2));
    return *workers;
  }

  void submit(std::function<void()> task) {
    {
      std::lock_guard<std::mutex> lock(mutex_);
      tasks_.push_back(std::move(task));
    }
    ready_.notify_one();
  }

private:
  explicit Workers(unsigned count) {
    for (unsigned i = 0; i < count; ++i) {
      std::thread([this] { run(); }).detach();
    }
  }

  [[noreturn]] void run() {
    for (;;) {
      std::function<void()> task;
      {
        std::unique_lock<std::mutex> lock(mutex_);
        ready_.wait(lock, [this] { return !tasks_.empty(); });
        task = std::move(tasks_.front());
        tasks_.pop_front();
      }
      task();
    }
  }

  std::mutex mutex_;
  std::condition_variable ready_;
  std::deque<std::function<void()>> tasks_;
};

/// The error string `lilia_c_open` or `lilia_c_close` handed over, freed.
std::string takeError(char *error) {
  if (error == nullptr) {
    return R"({"code":"INTERNAL","message":"the engine reported no error","retryable":false})";
  }
  std::string message(error);
  lilia_c_free_string(error);
  return message;
}

/// The JS frame of one call, owned until JavaScript drops the bytes it sees: those after the
/// JSON text (segment count, segment table, segments).
class Frame final : public jsi::MutableBuffer {
public:
  Frame(uint8_t *bytes, size_t length) : bytes_(bytes), length_(length) {}
  ~Frame() override { lilia_c_free_bytes(bytes_, length_); }
  Frame(const Frame &) = delete;
  Frame &operator=(const Frame &) = delete;

  size_t jsonLength() const {
    return static_cast<size_t>(bytes_[0]) | static_cast<size_t>(bytes_[1]) << 8 |
           static_cast<size_t>(bytes_[2]) << 16 | static_cast<size_t>(bytes_[3]) << 24;
  }
  const uint8_t *json() const { return bytes_ + 4; }
  size_t size() const override { return length_ - 4 - jsonLength(); }
  uint8_t *data() override { return bytes_ + 4 + jsonLength(); }

private:
  uint8_t *bytes_;
  size_t length_;
};

/// An open database. Calls share it; `close` waits for them, then releases the handle.
class Database {
public:
  explicit Database(LiliaHandle *handle) : handle_(handle) {}
  ~Database() {
    if (handle_ != nullptr) {
      lilia_c_close(handle_, kDropCloseTimeoutMs, nullptr);
    }
  }
  Database(const Database &) = delete;
  Database &operator=(const Database &) = delete;

  /// Runs the MessagePack operation `request` (`length` bytes).
  std::shared_ptr<Frame> execute(const uint8_t *request, size_t length) {
    std::shared_lock<std::shared_mutex> lock(mutex_);
    size_t frameLength = 0;
    uint8_t *frame = lilia_c_execute(handle_, request, length, 0, &frameLength);
    if (frame == nullptr) {
      throw std::runtime_error(
          R"({"code":"INVALID_INPUT","message":"the result exceeds 4 GiB","retryable":false})");
    }
    return std::make_shared<Frame>(frame, frameLength);
  }

  void close(uint64_t timeoutMs) {
    std::unique_lock<std::shared_mutex> lock(mutex_);
    if (handle_ == nullptr) {
      return;
    }
    char *error = nullptr;
    const int status = lilia_c_close(handle_, timeoutMs, &error);
    // `lilia_c_close` frees the handle whether or not the close succeeds.
    handle_ = nullptr;
    if (status != 0) {
      throw std::runtime_error(takeError(error));
    }
  }

private:
  std::shared_mutex mutex_;
  LiliaHandle *handle_;
};

/// The bytes of the `Uint8Array` `value`, valid until JavaScript runs again.
std::pair<const uint8_t *, size_t> viewOf(jsi::Runtime &runtime, const jsi::Value &value) {
  jsi::Object view = value.asObject(runtime);
  jsi::ArrayBuffer buffer = view.getProperty(runtime, "buffer").asObject(runtime).getArrayBuffer(runtime);
  const auto offset = static_cast<size_t>(view.getProperty(runtime, "byteOffset").asNumber());
  const auto length = static_cast<size_t>(view.getProperty(runtime, "byteLength").asNumber());
  if (offset + length > buffer.size(runtime)) {
    throw jsi::JSError(runtime, "the request is not a Uint8Array");
  }
  return {buffer.data(runtime) + offset, length};
}

/// A frame as JavaScript reads it: its JSON text and the bytes after it.
jsi::Value frameValue(jsi::Runtime &runtime, std::shared_ptr<Frame> frame) {
  jsi::String json = jsi::String::createFromUtf8(runtime, frame->json(), frame->jsonLength());
  jsi::ArrayBuffer tail(runtime, std::move(frame));
  return jsi::Array::createWithElements(runtime, std::move(json), std::move(tail));
}

/// The functions settling a promise, carried from the JavaScript thread to a worker and back.
struct Settle {
  jsi::Function resolve;
  jsi::Function reject;
};

/// A promise `start` receives the settling functions of.
jsi::Value makePromise(jsi::Runtime &runtime, std::function<void(std::shared_ptr<Settle>)> start) {
  jsi::Function executor = jsi::Function::createFromHostFunction(
      runtime, jsi::PropNameID::forAscii(runtime, "executor"), 2,
      [start = std::move(start)](jsi::Runtime &runtime, const jsi::Value &, const jsi::Value *args,
                                 size_t) -> jsi::Value {
        start(std::make_shared<Settle>(Settle{args[0].asObject(runtime).asFunction(runtime),
                                              args[1].asObject(runtime).asFunction(runtime)}));
        return jsi::Value::undefined();
      });
  return runtime.global().getPropertyAsFunction(runtime, "Promise").callAsConstructor(runtime, executor);
}

/// A promise of `work`, run on a worker: `finish` turns its result into the value the promise
/// resolves with, on the JavaScript thread; an exception of either rejects it.
template <typename T>
jsi::Value background(jsi::Runtime &runtime, const std::shared_ptr<CallInvoker> &invoker,
                      std::function<T()> work,
                      std::function<jsi::Value(jsi::Runtime &, T)> finish) {
  return makePromise(runtime, [invoker, work = std::move(work),
                               finish = std::move(finish)](std::shared_ptr<Settle> settle) mutable {
    Workers::shared().submit([invoker, work = std::move(work), finish = std::move(finish),
                              settle = std::move(settle)]() mutable {
      std::variant<T, std::string> outcome;
      try {
        outcome.template emplace<0>(work());
      } catch (const std::exception &error) {
        outcome.template emplace<1>(error.what());
      }
      // The JavaScript values travel back and are released on the JavaScript thread.
      invoker->invokeAsync([finish = std::move(finish), settle = std::move(settle),
                            outcome = std::move(outcome)](jsi::Runtime &runtime) mutable {
        try {
          if (outcome.index() == 1) {
            throw jsi::JSError(runtime, std::get<1>(outcome));
          }
          settle->resolve.call(runtime, finish(runtime, std::move(std::get<0>(outcome))));
        } catch (jsi::JSError &error) {
          settle->reject.call(runtime, error.value());
        } catch (const std::exception &error) {
          settle->reject.call(runtime, jsi::JSError(runtime, error.what()).value());
        }
      });
    });
  });
}

jsi::Function hostFunction(jsi::Runtime &runtime, const char *name, unsigned arguments,
                           jsi::HostFunctionType body) {
  return jsi::Function::createFromHostFunction(runtime, jsi::PropNameID::forAscii(runtime, name),
                                               arguments, std::move(body));
}

/// The object JavaScript holds for `database`.
jsi::Object databaseObject(jsi::Runtime &runtime, const std::shared_ptr<Database> &database,
                           const std::shared_ptr<CallInvoker> &invoker) {
  jsi::Object object(runtime);
  object.setProperty(
      runtime, "execute",
      hostFunction(runtime, "execute", 1,
                   [database, invoker](jsi::Runtime &runtime, const jsi::Value &,
                                       const jsi::Value *args, size_t count) -> jsi::Value {
                     if (count < 1) {
                       throw jsi::JSError(runtime, "execute takes a request");
                     }
                     const auto [bytes, length] = viewOf(runtime, args[0]);
                     // A copy: the request's memory belongs to JavaScript, which runs on.
                     auto request = std::make_shared<std::vector<uint8_t>>(bytes, bytes + length);
                     return background<std::shared_ptr<Frame>>(
                         runtime, invoker,
                         [database, request] { return database->execute(request->data(), request->size()); },
                         [](jsi::Runtime &runtime, std::shared_ptr<Frame> frame) {
                           return frameValue(runtime, std::move(frame));
                         });
                   }));
  object.setProperty(
      runtime, "executeSync",
      hostFunction(runtime, "executeSync", 1,
                   [database](jsi::Runtime &runtime, const jsi::Value &, const jsi::Value *args,
                              size_t count) -> jsi::Value {
                     if (count < 1) {
                       throw jsi::JSError(runtime, "executeSync takes a request");
                     }
                     const auto [bytes, length] = viewOf(runtime, args[0]);
                     try {
                       return frameValue(runtime, database->execute(bytes, length));
                     } catch (const std::runtime_error &error) {
                       throw jsi::JSError(runtime, error.what());
                     }
                   }));
  object.setProperty(
      runtime, "close",
      hostFunction(runtime, "close", 1,
                   [database, invoker](jsi::Runtime &runtime, const jsi::Value &,
                                       const jsi::Value *args, size_t count) -> jsi::Value {
                     const auto timeoutMs = static_cast<uint64_t>(
                         count > 0 && args[0].isNumber() ? std::max(0.0, args[0].asNumber()) : kDropCloseTimeoutMs);
                     return background<bool>(
                         runtime, invoker,
                         [database, timeoutMs] {
                           database->close(timeoutMs);
                           return true;
                         },
                         [](jsi::Runtime &, bool) { return jsi::Value::undefined(); });
                   }));
  return object;
}

} // namespace

void install(jsi::Runtime &runtime, std::shared_ptr<CallInvoker> invoker) {
  jsi::Object lilia(runtime);
  lilia.setProperty(
      runtime, "open",
      hostFunction(runtime, "open", 1,
                   [invoker](jsi::Runtime &runtime, const jsi::Value &, const jsi::Value *args,
                             size_t count) -> jsi::Value {
                     if (count < 1 || !args[0].isString()) {
                       throw jsi::JSError(runtime, "open takes the JSON of its options");
                     }
                     std::string options = args[0].asString(runtime).utf8(runtime);
                     return background<std::shared_ptr<Database>>(
                         runtime, invoker,
                         [options = std::move(options)] {
                           char *error = nullptr;
                           LiliaHandle *handle = lilia_c_open(options.c_str(), &error);
                           if (handle == nullptr) {
                             throw std::runtime_error(takeError(error));
                           }
                           return std::make_shared<Database>(handle);
                         },
                         [invoker](jsi::Runtime &runtime, std::shared_ptr<Database> database) -> jsi::Value {
                           return databaseObject(runtime, database, invoker);
                         });
                   }));
  runtime.global().setProperty(runtime, "__liliadb", std::move(lilia));
}

} // namespace liliadb
