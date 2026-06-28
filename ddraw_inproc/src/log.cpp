// 記錄器 —— 執行緒安全,append 到 DLL 同目錄 l38ddraw.log
#include "inhook.h"
#include <stdio.h>
#include <stdarg.h>

static CRITICAL_SECTION g_logCs;
static bool             g_logInit = false;
static char            g_logPath[MAX_PATH] = {0};

static void BuildLogPath()
{
    HMODULE hSelf = nullptr;
    GetModuleHandleExA(
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        (LPCSTR)&BuildLogPath, &hSelf);
    char dir[MAX_PATH] = {0};
    GetModuleFileNameA(hSelf, dir, MAX_PATH);
    char* slash = strrchr(dir, '\\');
    if (slash) *(slash + 1) = '\0';
    wsprintfA(g_logPath, "%sl38ddraw.log", dir);
}

static void EnsureLogInit()
{
    if (g_logInit) return;
    g_logInit = true;
    InitializeCriticalSection(&g_logCs);
    BuildLogPath();
    FILE* f = nullptr;
    fopen_s(&f, g_logPath, "w");
    if (f) {
        fprintf(f, "==== l38ddraw.dll in-process present 接管 log ====\n");
        fprintf(f, "pid=%lu\n", GetCurrentProcessId());
        fclose(f);
    }
}

void Log(const char* fmt, ...)
{
    EnsureLogInit();
    char buf[1024];
    va_list ap;
    va_start(ap, fmt);
    _vsnprintf_s(buf, sizeof(buf), _TRUNCATE, fmt, ap);
    va_end(ap);

    EnterCriticalSection(&g_logCs);
    FILE* f = nullptr;
    fopen_s(&f, g_logPath, "a");
    if (f) {
        fprintf(f, "[t%lu] %s\n", GetCurrentThreadId(), buf);
        fclose(f);
    }
    LeaveCriticalSection(&g_logCs);
}
