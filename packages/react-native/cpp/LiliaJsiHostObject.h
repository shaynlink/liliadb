#pragma once

#include <jsi/jsi.h>
#include <memory>
#include <string>
#include <vector>
#include "LiliaCAbi.h"

namespace liliadb {

class LiliaJsiHostObject : public facebook::jsi::HostObject {
public:
    explicit LiliaJsiHostObject(LiliaHandle* handle, std::string path);
    ~LiliaJsiHostObject() override;

    facebook::jsi::Value get(facebook::jsi::Runtime& runtime, const facebook::jsi::PropNameID& name) override;
    std::vector<facebook::jsi::PropNameID> getPropertyNames(facebook::jsi::Runtime& runtime) override;

    LiliaHandle* getHandle() const { return handle_; }
    const std::string& getPath() const { return path_; }

private:
    LiliaHandle* handle_;
    std::string path_;
    bool isClosed_;
};

} // namespace liliadb
