// ime.cpp —— 原生 IME 候選視窗修復(無自繪)
//
// 根因(Ghidra 靜態確認):
//   LUnicodeEdit 的 wndproc FUN_0059c6a0,WM_IME_SETCONTEXT(0x281)走自訂
//   handler FUN_0059ddb0。它在 IME 輸入模式下做 `lParam &= 0x3FFFFFF0`,strip 掉:
//     0x0000000F = ISC_SHOWUIALLCANDIDATEWINDOW(候選視窗 UI)
//     0x80000000 = ISC_SHOWUICOMPOSITIONWINDOW(組字視窗,遊戲自繪 inline)
//   → 系統(CUAS / 微軟新注音 TSF 橋接)被告知「候選 UI 別顯示」,所以即使
//     IMN_OPENCANDIDATE 一直發,CUAS 的 MSCTFIME UI 候選視窗永遠 vis=0。
//
// 解法(原生,非自繪):subclass LUnicodeEdit,攔 WM_IME_SETCONTEXT,取得焦點時
//   代為 DefWindowProc,「保留候選位元 0x0F、維持組字位元 0x80000000 關閉」。
//   → 候選視窗交回系統顯示(畫面已是 DWM 合成 swapchain,候選會被正確疊在上方);
//     組字仍由遊戲 inline 畫,不會重複。
//   WM_IME_NOTIFY 完全不碰:遊戲原 handler FUN_0059dea0 已正確 forward DefWindowProc
//   + ImmSetCandidateWindow 把候選位置設到游標,不需我們插手。
//
// 全部繁體中文註解(專案規則)。
#include "inhook.h"
#include <imm.h>

static const wchar_t* TARGET_CLASS = L"LUnicodeEdit";

// hwnd → 原 wndproc(聊天/對話輸入框數量少,固定陣列足夠)
struct SubEntry { HWND hwnd; WNDPROC orig; };
static SubEntry        g_subs[32] = {0};
static CRITICAL_SECTION g_subCs;
static bool             g_subCsInit = false;

static WNDPROC FindOrig(HWND h)
{
    WNDPROC r = nullptr;
    EnterCriticalSection(&g_subCs);
    for (int i = 0; i < 32; ++i)
        if (g_subs[i].hwnd == h) { r = g_subs[i].orig; break; }
    LeaveCriticalSection(&g_subCs);
    return r;
}

static bool IsTargetClass(HWND h)
{
    wchar_t buf[64];
    int n = GetClassNameW(h, buf, 64);
    return n > 0 && lstrcmpW(buf, TARGET_CLASS) == 0;
}

// 診斷 trace(全域上限,避免刷爆 log)。確認修復生效後可移除。
static volatile LONG g_imeLogN = 0;
static void LogImeMsg(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    const char* name = nullptr;
    switch (msg) {
        case WM_IME_SETCONTEXT:       name = "SETCONTEXT"; break;
        case WM_IME_NOTIFY:           name = "NOTIFY"; break;
        default: return;
    }
    if (InterlockedIncrement(&g_imeLogN) <= 40)
        Log("ime-trace: %s hwnd=%p wp=0x%IX lp=0x%IX", name, hwnd, wp, lp);
}

static LRESULT CALLBACK SubclassProc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    WNDPROC orig = FindOrig(hwnd);

    LogImeMsg(hwnd, msg, wp, lp);

    // 取得焦點(wp != 0)的 WM_IME_SETCONTEXT:遊戲 FUN_0059ddb0 會 strip 候選位元
    //   → 候選視窗永不顯示。代為 DefWindowProc,保留候選位元(0x0F),組字位元
    //   (0x80000000)維持關閉(遊戲自繪 inline,開了會重複)。
    //   遊戲原 handler 只呼叫 DefWindowProcA、無其他狀態更新,直接取代安全。
    if (msg == WM_IME_SETCONTEXT && wp != 0) {
        LPARAM fixed = (lp & 0x3FFFFFF0) | ISC_SHOWUIALLCANDIDATEWINDOW;
        static volatile LONG once = 0;
        if (InterlockedExchange((LONG*)&once, 1) == 0)
            Log("ime: SETCONTEXT 候選位元修復生效 hwnd=%p lp=0x%IX -> 0x%IX",
                hwnd, lp, fixed);
        return DefWindowProcW(hwnd, msg, wp, fixed);
    }

    if (orig) return CallWindowProcW(orig, hwnd, msg, wp, lp);
    return DefWindowProcW(hwnd, msg, wp, lp);
}

static void SubclassEdit(HWND h)
{
    if (!IsTargetClass(h)) return;

    bool newly = false;
    EnterCriticalSection(&g_subCs);
    bool already = false;
    int  slot = -1;
    for (int i = 0; i < 32; ++i) {
        if (g_subs[i].hwnd == h) { already = true; break; }
        if (slot < 0 && g_subs[i].hwnd == nullptr) slot = i;
    }
    if (!already && slot >= 0) {
        WNDPROC orig = (WNDPROC)(uintptr_t)GetWindowLongPtrW(h, GWLP_WNDPROC);
        if (orig && orig != SubclassProc) {
            g_subs[slot].hwnd = h;
            g_subs[slot].orig = orig;
            SetWindowLongPtrW(h, GWLP_WNDPROC, (LONG_PTR)(uintptr_t)SubclassProc);
            newly = true;
        }
    }
    LeaveCriticalSection(&g_subCs);

    if (newly) Log("ime: subclass LUnicodeEdit hwnd=%p", h);
}

// WinEvent OUTOFCONTEXT callback(跑在 worker thread,有 message loop)。
// 玩家點聊天框(OBJECT_FOCUS)或遊戲新建輸入框(OBJECT_CREATE)時 subclass。
static void CALLBACK WinEventProc(HWINEVENTHOOK, DWORD event, HWND hwnd,
                                  LONG idObject, LONG, DWORD, DWORD)
{
    if ((event == EVENT_OBJECT_CREATE || event == EVENT_OBJECT_FOCUS) &&
        idObject == 0 && hwnd) {
        SubclassEdit(hwnd);
    }
}

void InstallImeRouting()
{
    if (!g_subCsInit) { InitializeCriticalSection(&g_subCs); g_subCsInit = true; }
    // 不能用 WINEVENT_SKIPOWNPROCESS — 我們注入在目標 process,自己 process 就是目標。
    HWINEVENTHOOK hk = SetWinEventHook(
        EVENT_OBJECT_CREATE, EVENT_OBJECT_FOCUS, NULL,
        WinEventProc, GetCurrentProcessId(), 0, WINEVENT_OUTOFCONTEXT);
    Log("ime: WinEventHook installed=%p", hk);
}
