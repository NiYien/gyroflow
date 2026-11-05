// SPDX-License-Identifier: GPL-3.0-or-later
#include <ofxCore.h>
#include <ofxImageEffect.h>
#include <ofxParam.h>
#include <ofxProperty.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <mutex>
#include <dlfcn.h>

static OfxHost* host = nullptr;
static const OfxPropertySuiteV1* properties = nullptr;
static const OfxImageEffectSuiteV1* images = nullptr;
static const OfxParameterSuiteV1* parameters = nullptr;
static std::mutex log_mutex;

static void record(const char* key, const char* value) {
    std::lock_guard<std::mutex> lock(log_mutex);
    const char* home = std::getenv("HOME");
    if (!home) return;
    std::error_code error;
    auto directory = std::filesystem::path(home) / "Library/Logs/Gyroflow";
    std::filesystem::create_directories(directory, error);
    const auto filename = (directory / "jianying-ofx-probe.log").string();
    FILE* file = std::fopen(filename.c_str(), "a");
    if (file) {
        std::fprintf(file, "%s=%s\n", key, value ? value : "(null)");
        std::fclose(file);
    }
}

static void inspect(OfxPropertySetHandle set, const char* scope, const char* name) {
    if (!properties || !set) return;
    int count = 0;
    if (properties->propGetDimension(set, name, &count) != kOfxStatOK) return;
    for (int index = 0; index < count; ++index) {
        char key[256], value[128];
        std::snprintf(key, sizeof(key), "%s.%s[%d]", scope, name, index);
        const char* text = nullptr;
        int integer = 0;
        double real = 0;
        void* pointer = nullptr;
        if (properties->propGetString(set, name, index, &text) == kOfxStatOK) record(key, text);
        else if (properties->propGetInt(set, name, index, &integer) == kOfxStatOK) {
            std::snprintf(value, sizeof(value), "%d", integer); record(key, value);
        } else if (properties->propGetDouble(set, name, index, &real) == kOfxStatOK) {
            std::snprintf(value, sizeof(value), "%.17g", real); record(key, value);
        } else if (properties->propGetPointer(set, name, index, &pointer) == kOfxStatOK) {
            std::snprintf(value, sizeof(value), "%p", pointer); record(key, value);
        }
    }
}

static void inspect_gpu(OfxPropertySetHandle set, const char* scope) {
    for (auto name : {"OfxImageEffectPropOpenGLRenderSupported", "OfxImageEffectPropOpenGLEnabled",
        "OfxImageEffectPropOpenCLRenderSupported", "OfxImageEffectPropOpenCLEnabled",
        "OfxImageEffectPropOpenCLCommandQueue", "OfxImageEffectPropMetalRenderSupported",
        "OfxImageEffectPropMetalEnabled", "OfxImageEffectPropMetalCommandQueue",
        "OfxImageEffectPropCudaRenderSupported", "OfxImageEffectPropCudaEnabled",
        "OfxImageEffectPropCudaStream", "OfxImageEffectPropRenderScale", "OfxPropTime"}) {
        inspect(set, scope, name);
    }
}

static void set_host(OfxHost* value) { host = value; }

static OfxStatus action(const char* name, const void* handle, OfxPropertySetHandle in, OfxPropertySetHandle) {
    record("action", name);
    if (!std::strcmp(name, kOfxActionLoad)) {
        properties = static_cast<const OfxPropertySuiteV1*>(host->fetchSuite(host->host, kOfxPropertySuite, 1));
        images = static_cast<const OfxImageEffectSuiteV1*>(host->fetchSuite(host->host, kOfxImageEffectSuite, 1));
        parameters = static_cast<const OfxParameterSuiteV1*>(host->fetchSuite(host->host, kOfxParameterSuite, 1));
        if (!properties || !images || !parameters) return kOfxStatErrMissingHostFeature;
        Dl_info module{};
        if (dladdr(reinterpret_cast<void*>(&action), &module)) record("loaded_binary", module.dli_fname);
        record("architecture", sizeof(void*) == 8 ? "64-bit" : "32-bit");
        for (auto property : {kOfxPropName, kOfxPropLabel, kOfxPropVersion, kOfxPropVersionLabel,
            "OfxImageEffectHostPropSupportsCustomInteract", "OfxImageEffectPropSupportsOverlays",
            "OfxImageEffectPropSupportedPixelDepths", "OfxImageEffectPropSupportedContexts"}) {
            inspect(host->host, "host", property);
        }
        inspect_gpu(host->host, "host");
        return kOfxStatOK;
    }
    auto effect = reinterpret_cast<OfxImageEffectHandle>(const_cast<void*>(handle));
    if (!std::strcmp(name, kOfxActionDescribe)) {
        OfxPropertySetHandle descriptor = nullptr;
        images->getPropertySet(effect, &descriptor);
        properties->propSetString(descriptor, kOfxPropLabel, 0, "Gyroflow Jianying capability probe");
        properties->propSetString(descriptor, kOfxImageEffectPropSupportedContexts, 0, kOfxImageEffectContextFilter);
        properties->propSetString(descriptor, kOfxImageEffectPropSupportedPixelDepths, 0, kOfxBitDepthByte);
        properties->propSetString(descriptor, kOfxImageEffectPropSupportedPixelDepths, 1, kOfxBitDepthFloat);
        for (auto property : {"OfxImageEffectPropMetalRenderSupported", "OfxImageEffectPropOpenGLRenderSupported", "OfxImageEffectPropOpenCLRenderSupported"}) {
            const char* supported = nullptr;
            if (properties->propGetString(host->host, property, 0, &supported) == kOfxStatOK)
                properties->propSetString(descriptor, property, 0, supported);
        }
        return kOfxStatOK;
    }
    if (!std::strcmp(name, kOfxImageEffectActionDescribeInContext)) {
        for (auto clip : {kOfxImageEffectSimpleSourceClipName, kOfxImageEffectOutputClipName}) {
            OfxPropertySetHandle descriptor = nullptr;
            images->clipDefine(effect, clip, &descriptor);
            properties->propSetString(descriptor, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA);
        }
        OfxParamSetHandle set = nullptr;
        images->getParamSet(effect, &set);
        OfxPropertySetHandle parameter = nullptr;
        parameters->paramDefine(set, kOfxParamTypeDouble, "ProbeValue", &parameter);
        properties->propSetDouble(parameter, kOfxParamPropDefault, 0, 48.0);
        parameters->paramDefine(set, kOfxParamTypeString, "ProbeLabel", &parameter);
        properties->propSetString(parameter, kOfxParamPropStringMode, 0, kOfxParamStringIsLabel);
        properties->propSetString(parameter, kOfxParamPropDefault, 0, "Diagnostic only: no stabilized output");
        parameters->paramDefine(set, kOfxParamTypePushButton, "ProbeButton", &parameter);
        parameters->paramDefine(set, kOfxParamTypePage, "Controls", &parameter);
        for (int i = 0; i < 3; ++i) properties->propSetString(parameter, kOfxParamPropPageChild, i,
            i == 0 ? "ProbeValue" : i == 1 ? "ProbeLabel" : "ProbeButton");
        return kOfxStatOK;
    }
    inspect_gpu(in, "action");
    for (auto property : {kOfxPropName, kOfxPropChangeReason, kOfxPropType}) inspect(in, "action", property);
    if (handle && (!std::strcmp(name, kOfxActionBeginInstanceChanged)
        || !std::strcmp(name, kOfxActionInstanceChanged) || !std::strcmp(name, kOfxActionEndInstanceChanged))) {
        OfxParamSetHandle set = nullptr;
        images->getParamSet(effect, &set);
        OfxParamHandle parameter = nullptr;
        double value = 0;
        int counter = 0;
        char text[128];
        if (parameters->paramGetHandle(set, "ProbeValue", &parameter, nullptr) == kOfxStatOK
            && parameters->paramGetValue(parameter, &value) == kOfxStatOK) {
            std::snprintf(text, sizeof(text), "%.17g", value); record("parameter.ProbeValue", text);
        }
        if (parameters->paramGetHandle(set, "ProbeButton", &parameter, nullptr) == kOfxStatOK
            && parameters->paramGetValue(parameter, &counter) == kOfxStatOK) {
            std::snprintf(text, sizeof(text), "%d", counter); record("parameter.ProbeButton", text);
        }
    }
    if (!std::strcmp(name, kOfxActionCreateInstance) || !std::strcmp(name, kOfxActionDestroyInstance)
        || !std::strcmp(name, kOfxActionBeginInstanceChanged) || !std::strcmp(name, kOfxActionInstanceChanged)
        || !std::strcmp(name, kOfxActionEndInstanceChanged)) return kOfxStatOK;
    if (!std::strcmp(name, kOfxImageEffectActionRender)) {
        // Deliberately fail instead of presenting a CPU fallback as GPU support.
        record("render", "contract captured; diagnostic probe does not produce frames");
        return kOfxStatErrUnsupported;
    }
    return kOfxStatReplyDefault;
}

static OfxPlugin plugin = {kOfxImageEffectPluginApi, 1, "xyz.niyien.gyroflow.jianying.macos.capability-probe", 0, 1, set_host, action};
extern "C" __attribute__((visibility("default"))) int OfxGetNumberOfPlugins() { return 1; }
extern "C" __attribute__((visibility("default"))) OfxPlugin* OfxGetPlugin(int index) { return index == 0 ? &plugin : nullptr; }
