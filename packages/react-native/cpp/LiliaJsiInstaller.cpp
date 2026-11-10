#include "LiliaJsiInstaller.h"
#include "LiliaJsiHostObject.h"
#include "LiliaCAbi.h"
#include <vector>

namespace liliadb {

using namespace facebook;

void installLiliaJsi(jsi::Runtime& runtime) {
    auto openSyncFunc = jsi::Function::createFromHostFunction(
        runtime,
        jsi::PropNameID::forUtf8(runtime, "__lilia_open_sync"),
        1,
        [](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
            if (count < 1 || !args[0].isObject()) {
                throw jsi::JSError(rt, "__lilia_open_sync expects options object");
            }

            auto opts = args[0].asObject(rt);
            if (!opts.hasProperty(rt, "path") || !opts.getProperty(rt, "path").isString()) {
                throw jsi::JSError(rt, "path is required");
            }

            std::string path = opts.getProperty(rt, "path").asString(rt).utf8(rt);

            int profileCode = 0; // 0=Mobile, 1=Constrained, 2=Server
            if (opts.hasProperty(rt, "profile") && opts.getProperty(rt, "profile").isString()) {
                std::string p = opts.getProperty(rt, "profile").asString(rt).utf8(rt);
                if (p == "constrained") profileCode = 1;
                else if (p == "server") profileCode = 2;
            }

            const char* passphrasePtr = nullptr;
            std::string passphrase;
            if (opts.hasProperty(rt, "passphrase") && opts.getProperty(rt, "passphrase").isString()) {
                passphrase = opts.getProperty(rt, "passphrase").asString(rt).utf8(rt);
                passphrasePtr = passphrase.c_str();
            }

            std::vector<uint8_t> keyBytes;
            if (opts.hasProperty(rt, "encryptionKeyHex") && opts.getProperty(rt, "encryptionKeyHex").isString()) {
                std::string hex = opts.getProperty(rt, "encryptionKeyHex").asString(rt).utf8(rt);
                if (hex.size() == 64) {
                    keyBytes.reserve(32);
                    for (size_t i = 0; i < 64; i += 2) {
                        uint8_t byte = static_cast<uint8_t>(std::stoi(hex.substr(i, 2), nullptr, 16));
                        keyBytes.push_back(byte);
                    }
                }
            }

            char* outErr = nullptr;
            LiliaHandle* handle = lilia_c_open(
                path.c_str(),
                profileCode,
                keyBytes.empty() ? nullptr : keyBytes.data(),
                keyBytes.size(),
                passphrasePtr,
                &outErr
            );

            if (handle == nullptr) {
                std::string err = outErr ? outErr : "Unknown error opening database";
                if (outErr) lilia_c_free_string(outErr);
                throw jsi::JSError(rt, err);
            }

            auto hostObj = std::make_shared<LiliaJsiHostObject>(handle, path);
            return jsi::Object::createFromHostObject(rt, hostObj);
        }
    );

    runtime.global().setProperty(runtime, "__lilia_open_sync", openSyncFunc);
}

} // namespace liliadb
