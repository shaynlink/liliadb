#include "LiliaJsiHostObject.h"
#include <iostream>

namespace liliadb {

using namespace facebook;

LiliaJsiHostObject::LiliaJsiHostObject(LiliaHandle* handle, std::string path)
    : handle_(handle), path_(std::move(path)), isClosed_(false) {}

LiliaJsiHostObject::~LiliaJsiHostObject() {
    if (!isClosed_ && handle_ != nullptr) {
        lilia_c_close(handle_, 1000);
        lilia_c_free_handle(handle_);
        handle_ = nullptr;
        isClosed_ = true;
    }
}

std::vector<jsi::PropNameID> LiliaJsiHostObject::getPropertyNames(jsi::Runtime& runtime) {
    std::vector<std::string> names = {
        "path", "isClosed",
        "kvGet", "kvSet", "kvDelete",
        "jsonGet", "jsonPut", "jsonDelete", "jsonFind",
        "graphSetNode", "graphGetNode", "graphDeleteNode",
        "graphAddEdge", "graphRemoveEdge", "graphNeighbors", "graphReachability",
        "ftsIndex", "ftsDelete", "ftsSearch",
        "sqlQuery",
        "checkpoint", "vacuum", "getFileSize", "backup", "close"
    };
    std::vector<jsi::PropNameID> result;
    result.reserve(names.size());
    for (const auto& name : names) {
        result.push_back(jsi::PropNameID::forUtf8(runtime, name));
    }
    return result;
}

jsi::Value LiliaJsiHostObject::get(jsi::Runtime& runtime, const jsi::PropNameID& name) {
    auto propName = name.utf8(runtime);

    if (propName == "path") {
        return jsi::String::createFromUtf8(runtime, path_);
    }
    if (propName == "isClosed") {
        return jsi::Value(isClosed_);
    }

    // ==========================================
    // KEY / VALUE JSI
    // ==========================================
    if (propName == "kvGet") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 2,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                if (count < 2) throw jsi::JSError(rt, "kvGet expects (namespace, key)");

                std::string ns = args[0].asString(rt).utf8(rt);
                std::string key = args[1].asString(rt).utf8(rt);

                size_t valLen = 0;
                uint64_t version = 0;
                int64_t expires = 0;

                uint8_t* valBytes = lilia_c_kv_get(
                    handle_, ns.c_str(),
                    reinterpret_cast<const uint8_t*>(key.data()), key.size(),
                    &valLen, &version, &expires
                );

                if (valBytes == nullptr) {
                    return jsi::Value::null();
                }

                std::string valStr(reinterpret_cast<char*>(valBytes), valLen);
                lilia_c_free_bytes(valBytes, valLen);

                return jsi::String::createFromUtf8(rt, valStr);
            }
        );
    }

    if (propName == "kvSet") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 4,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                if (count < 3) throw jsi::JSError(rt, "kvSet expects (namespace, key, value, [expiresAtMs])");

                std::string ns = args[0].asString(rt).utf8(rt);
                std::string key = args[1].asString(rt).utf8(rt);
                std::string val = args[2].asString(rt).utf8(rt);
                int64_t expiresAt = (count >= 4 && !args[3].isNull() && !args[3].isUndefined())
                                        ? static_cast<int64_t>(args[3].asNumber())
                                        : 0;

                uint64_t version = 0;
                int rc = lilia_c_kv_set(
                    handle_, ns.c_str(),
                    reinterpret_cast<const uint8_t*>(key.data()), key.size(),
                    reinterpret_cast<const uint8_t*>(val.data()), val.size(),
                    expiresAt, &version
                );

                if (rc != 0) {
                    throw jsi::JSError(rt, "Failed to execute kvSet");
                }
                return jsi::Value(static_cast<double>(version));
            }
        );
    }

    if (propName == "kvDelete") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 2,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                std::string ns = args[0].asString(rt).utf8(rt);
                std::string key = args[1].asString(rt).utf8(rt);

                int deleted = 0;
                lilia_c_kv_delete(
                    handle_, ns.c_str(),
                    reinterpret_cast<const uint8_t*>(key.data()), key.size(),
                    &deleted
                );
                return jsi::Value(deleted == 1);
            }
        );
    }

    // ==========================================
    // JSON DOCUMENT JSI
    // ==========================================
    if (propName == "jsonGet") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 2,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                std::string space = args[0].asString(rt).utf8(rt);
                std::string id = args[1].asString(rt).utf8(rt);

                uint64_t version = 0;
                char* jsonStr = lilia_c_json_get(handle_, space.c_str(), id.c_str(), &version);
                if (jsonStr == nullptr) {
                    return jsi::Value::null();
                }

                std::string s(jsonStr);
                lilia_c_free_string(jsonStr);

                return jsi::String::createFromUtf8(rt, s);
            }
        );
    }

    if (propName == "jsonPut") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 3,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                std::string space = args[0].asString(rt).utf8(rt);
                std::string id = args[1].asString(rt).utf8(rt);
                std::string jsonVal = args[2].asString(rt).utf8(rt);

                uint64_t version = 0;
                int rc = lilia_c_json_put(handle_, space.c_str(), id.c_str(), jsonVal.c_str(), &version);
                if (rc != 0) {
                    throw jsi::JSError(rt, "Failed to execute jsonPut");
                }
                return jsi::Value(static_cast<double>(version));
            }
        );
    }

    if (propName == "jsonDelete") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 2,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                std::string space = args[0].asString(rt).utf8(rt);
                std::string id = args[1].asString(rt).utf8(rt);

                int deleted = 0;
                lilia_c_json_delete(handle_, space.c_str(), id.c_str(), &deleted);
                return jsi::Value(deleted == 1);
            }
        );
    }

    if (propName == "jsonFind") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 4,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                std::string space = args[0].asString(rt).utf8(rt);
                std::string queryJson = args[1].asString(rt).utf8(rt);
                const char* afterId = (count >= 3 && args[2].isString()) ? args[2].asString(rt).utf8(rt).c_str() : nullptr;
                uint32_t limit = (count >= 4 && args[3].isNumber()) ? static_cast<uint32_t>(args[3].asNumber()) : 50;

                char* outJson = nullptr;
                int rc = lilia_c_json_find(handle_, space.c_str(), queryJson.c_str(), afterId, limit, &outJson);
                if (rc != 0 || outJson == nullptr) {
                    return jsi::String::createFromUtf8(rt, "[]");
                }
                std::string s(outJson);
                lilia_c_free_string(outJson);
                return jsi::String::createFromUtf8(rt, s);
            }
        );
    }

    // ==========================================
    // GRAPH JSI
    // ==========================================
    if (propName == "graphSetNode") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 4,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                std::string graph = args[0].asString(rt).utf8(rt);
                std::string id = args[1].asString(rt).utf8(rt);
                std::string kind = args[2].asString(rt).utf8(rt);
                std::string metaJson = args[3].asString(rt).utf8(rt);

                lilia_c_graph_set_node(handle_, graph.c_str(), id.c_str(), kind.c_str(), metaJson.c_str());
                return jsi::Value::undefined();
            }
        );
    }

    if (propName == "graphGetNode") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 2,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                std::string graph = args[0].asString(rt).utf8(rt);
                std::string id = args[1].asString(rt).utf8(rt);

                char* outJson = nullptr;
                lilia_c_graph_get_node(handle_, graph.c_str(), id.c_str(), &outJson);
                if (outJson == nullptr) return jsi::Value::null();
                std::string s(outJson);
                lilia_c_free_string(outJson);
                return jsi::String::createFromUtf8(rt, s);
            }
        );
    }

    if (propName == "graphAddEdge") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 4,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                std::string graph = args[0].asString(rt).utf8(rt);
                std::string source = args[1].asString(rt).utf8(rt);
                std::string target = args[2].asString(rt).utf8(rt);
                std::string kind = args[3].asString(rt).utf8(rt);

                lilia_c_graph_add_edge(handle_, graph.c_str(), source.c_str(), target.c_str(), kind.c_str());
                return jsi::Value::undefined();
            }
        );
    }

    if (propName == "graphNeighbors") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 4,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                std::string graph = args[0].asString(rt).utf8(rt);
                std::string node = args[1].asString(rt).utf8(rt);
                int dir = static_cast<int>(args[2].asNumber());
                const char* kind = (count >= 4 && args[3].isString()) ? args[3].asString(rt).utf8(rt).c_str() : nullptr;

                char* outJson = nullptr;
                lilia_c_graph_neighbors(handle_, graph.c_str(), node.c_str(), dir, kind, &outJson);
                if (outJson == nullptr) return jsi::String::createFromUtf8(rt, "[]");
                std::string s(outJson);
                lilia_c_free_string(outJson);
                return jsi::String::createFromUtf8(rt, s);
            }
        );
    }

    // ==========================================
    // FTS5 JSI
    // ==========================================
    if (propName == "ftsSearch") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 3,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                std::string col = args[0].asString(rt).utf8(rt);
                std::string query = args[1].asString(rt).utf8(rt);
                uint32_t limit = count >= 3 ? static_cast<uint32_t>(args[2].asNumber()) : 20;

                char* outJson = nullptr;
                lilia_c_fts_search(handle_, col.c_str(), query.c_str(), limit, &outJson);
                if (outJson == nullptr) return jsi::String::createFromUtf8(rt, "[]");
                std::string s(outJson);
                lilia_c_free_string(outJson);
                return jsi::String::createFromUtf8(rt, s);
            }
        );
    }

    // ==========================================
    // SQL JSI
    // ==========================================
    if (propName == "sqlQuery") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 2,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                std::string sql = args[0].asString(rt).utf8(rt);
                const char* paramsJson = (count >= 2 && args[1].isString()) ? args[1].asString(rt).utf8(rt).c_str() : nullptr;

                char* outCols = nullptr;
                char* outRows = nullptr;
                uint64_t affected = 0;

                int rc = lilia_c_sql_query(handle_, sql.c_str(), paramsJson, &outCols, &outRows, &affected);
                if (rc != 0) {
                    throw jsi::JSError(rt, "Failed to execute sqlQuery");
                }

                auto obj = jsi::Object(rt);
                obj.setProperty(rt, "columnsJson", jsi::String::createFromUtf8(rt, outCols ? outCols : "[]"));
                obj.setProperty(rt, "rowsJson", jsi::String::createFromUtf8(rt, outRows ? outRows : "[]"));
                obj.setProperty(rt, "rowsAffected", jsi::Value(static_cast<double>(affected)));

                if (outCols) lilia_c_free_string(outCols);
                if (outRows) lilia_c_free_string(outRows);

                return obj;
            }
        );
    }

    // ==========================================
    // MAINTENANCE JSI
    // ==========================================
    if (propName == "vacuum") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 0,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                lilia_c_vacuum(handle_);
                return jsi::Value::undefined();
            }
        );
    }

    if (propName == "getFileSize") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 0,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                uint64_t mainBytes = 0, walBytes = 0, shmBytes = 0;
                lilia_c_get_file_size(handle_, &mainBytes, &walBytes, &shmBytes);

                auto obj = jsi::Object(rt);
                obj.setProperty(rt, "mainBytes", jsi::Value(static_cast<double>(mainBytes)));
                obj.setProperty(rt, "walBytes", jsi::Value(static_cast<double>(walBytes)));
                obj.setProperty(rt, "shmBytes", jsi::Value(static_cast<double>(shmBytes)));
                obj.setProperty(rt, "totalBytes", jsi::Value(static_cast<double>(mainBytes + walBytes + shmBytes)));
                return obj;
            }
        );
    }

    if (propName == "checkpoint") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 0,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (isClosed_ || handle_ == nullptr) throw jsi::JSError(rt, "Database is closed");
                lilia_c_checkpoint(handle_);
                return jsi::Value::undefined();
            }
        );
    }

    if (propName == "close") {
        return jsi::Function::createFromHostFunction(
            runtime, name, 1,
            [this](jsi::Runtime& rt, const jsi::Value& thisVal, const jsi::Value* args, size_t count) -> jsi::Value {
                if (!isClosed_ && handle_ != nullptr) {
                    uint64_t timeout = count >= 1 ? static_cast<uint64_t>(args[0].asNumber()) : 1000;
                    lilia_c_close(handle_, timeout);
                    lilia_c_free_handle(handle_);
                    handle_ = nullptr;
                    isClosed_ = true;
                }
                return jsi::Value::undefined();
            }
        );
    }

    return jsi::Value::undefined();
}

} // namespace liliadb
