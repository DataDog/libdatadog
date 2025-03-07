#include <windows.h>
#include <WerApi.h>
#include <Psapi.h>
#include <vector>
#include <string>
#include <regex>

std::wstring php_module_path;
std::wstring tracer_module_path;

HMODULE php_module;
HMODULE tracer_module;

extern "C"
{
    HRESULT __declspec(dllexport) OutOfProcessExceptionEventCallback(
        PVOID pContext,
        const PWER_RUNTIME_EXCEPTION_INFORMATION pExceptionInformation,
        BOOL* pbOwnershipClaimed,
        PWSTR pwszEventName,
        PDWORD pchSize,
        PDWORD pdwSignatureCount
    )
    {
        OutputDebugStringW(L"Datadog Crashtracking - OutOfProcessExceptionEventCallback");

        auto process = pExceptionInformation->hProcess;

        DWORD cbNeeded;

        if (!EnumProcessModules(process, nullptr, 0, &cbNeeded))
        {
            OutputDebugStringW(L"Failed to enumerate process modules (1st)");
            return E_FAIL;
        }

        auto modules = std::vector<HMODULE>(cbNeeded / sizeof(HMODULE));

        if (!EnumProcessModules(process, modules.data(), cbNeeded, &cbNeeded))
        {
            OutputDebugStringW(L"Failed to enumerate process modules (2nd)");
            return E_FAIL;
        }

        std::wregex php_pattern(LR"(php\d+(ts|nts)\.dll$)", std::regex_constants::icase);

        for (auto module : modules)
        {
            wchar_t module_name[MAX_PATH];
            if (!GetModuleFileNameExW(process, module, module_name, MAX_PATH))
            {
                continue;
            }

            if (std::regex_search(module_name, php_pattern))
            {
                php_module_path = module_name;
                OutputDebugStringW((L"Found php module: " + php_module_path).c_str());
            }

            if (wcsstr(module_name, L"php_ddtrace.dll") != nullptr)
            {
                tracer_module_path = module_name;
                OutputDebugStringW((L"Found tracer module: " + tracer_module_path).c_str());
            }
        }

        if (php_module_path.empty() || tracer_module_path.empty())
        {
            OutputDebugStringW(L"Failed to find php or tracer module");
            return E_FAIL;
        }

        php_module = LoadLibraryW(php_module_path.c_str());

        if (php_module == NULL)
        {
            OutputDebugStringW(L"Failed to load php module");
            return E_FAIL;
        }

        tracer_module = LoadLibraryW(tracer_module_path.c_str());

        if (tracer_module == NULL)
        {
            OutputDebugStringW(L"Failed to load tracer module");
            return E_FAIL;
        }

        auto callback = (HRESULT(*)(PVOID, const PWER_RUNTIME_EXCEPTION_INFORMATION, BOOL*, PWSTR, PDWORD, PDWORD))GetProcAddress(tracer_module, "OutOfProcessExceptionEventCallback");

        if (callback == NULL)
        {
            OutputDebugStringW(L"Failed to load callback");
            return E_FAIL;
        }

        return callback(pContext, pExceptionInformation, pbOwnershipClaimed, pwszEventName, pchSize, pdwSignatureCount);
    }

    HRESULT __declspec(dllexport) OutOfProcessExceptionEventSignatureCallback(
        PVOID pContext,
        const PWER_RUNTIME_EXCEPTION_INFORMATION pExceptionInformation,
        DWORD dwIndex,
        PWSTR pwszName,
        PDWORD pchName,
        PWSTR pwszValue,
        PDWORD pchValue
    )
    {
        _Unreferenced_parameter_(pContext);
        _Unreferenced_parameter_(pExceptionInformation);
        _Unreferenced_parameter_(dwIndex);
        _Unreferenced_parameter_(pwszName);
        _Unreferenced_parameter_(pchName);
        _Unreferenced_parameter_(pwszValue);
        _Unreferenced_parameter_(pchValue);

        return E_NOTIMPL;
    }

    HRESULT __declspec(dllexport) OutOfProcessExceptionEventDebuggerLaunchCallback(
        PVOID pContext,
        const PWER_RUNTIME_EXCEPTION_INFORMATION pExceptionInformation,
        PBOOL pbIsCustomDebugger,
        PWSTR pwszDebuggerLaunch,
        PDWORD pchDebuggerLaunch,
        PBOOL pbIsDebuggerAutolaunch
    )
    {
        _Unreferenced_parameter_(pContext);
        _Unreferenced_parameter_(pExceptionInformation);
        _Unreferenced_parameter_(pbIsCustomDebugger);
        _Unreferenced_parameter_(pwszDebuggerLaunch);
        _Unreferenced_parameter_(pchDebuggerLaunch);
        _Unreferenced_parameter_(pbIsDebuggerAutolaunch);

        return E_NOTIMPL;
    }
}
