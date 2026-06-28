// main.cpp —— DllMain + worker + AOB 定位 + 內聯 hook + present detour
//
// 流程:
//  1. DllMain(ATTACH)→ 開 worker thread(避 loader lock)。
//  2. worker 輪詢等 ddraw init 完成(primary 0x9a84e8 非 null)。
//  3. AOB 定位 present 函式 FUN_0055d460(live build 可能整段 +0x390 偏移,故不寫死)。
//  4. 內聯 hook:把原生 present 改走 PresentDetour → DXGI swapchain。
//
// 全部繁體中文註解(專案規則)。
#include "inhook.h"

// present 偵測就緒旗標(detour 用)
static volatile bool g_ready = false;

// ───────────────────── present 函式簽章 ─────────────────────
//  FUN_0055d460(undefined4 src_surface, undefined4 src_rect, int* dst_origin_xy) __cdecl
//   視窗化:DAT_009a84e8(primary)->Blt(dstScreenRect, src, src_rect, DDBLT_WAIT, 0)
typedef void(__cdecl* PFN_Present)(IDirectDrawSurface7*, RECT*, int*);
static void* g_origPresent = nullptr;   // trampoline(可當原函式呼叫)

// ───────────────────── 遊戲主視窗 HWND ─────────────────────
static HWND g_gameHwnd = nullptr;

static BOOL CALLBACK EnumThreadWndProc(HWND h, LPARAM lp)
{
    if (!IsWindowVisible(h)) return TRUE;
    if (GetWindow(h, GW_OWNER)) return TRUE;          // 跳過 owned(對話框等)
    if (GetWindowLongA(h, GWL_STYLE) & WS_CHILD) return TRUE;
    *(HWND*)lp = h;
    return FALSE;
}

static HWND GetGameHwnd()
{
    if (g_gameHwnd && IsWindow(g_gameHwnd)) return g_gameHwnd;
    HWND found = nullptr;
    // present 跑在 render/UI thread = 擁有遊戲視窗的 thread → 列舉該 thread 頂層視窗
    EnumThreadWindows(GetCurrentThreadId(), EnumThreadWndProc, (LPARAM)&found);
    if (!found) found = FindWindowA(NULL, "Lineage Windows Client (13081901)");
    g_gameHwnd = found;
    return found;
}

// ───────────────────── present detour ─────────────────────
//  回傳 true = 已用 swapchain present(跳過原生 Blt);false = 交回原函式。
static bool TryPresentSwapchain(IDirectDrawSurface7* src, RECT* srcRect)
{
    if (!g_ready || !src) return false;
    if (*(volatile BYTE*)gaddr::FULLSCREEN_FLAG != 0) return false;  // 全螢幕走原生
    HWND hwnd = GetGameHwnd();
    if (!hwnd) return false;

    DDSURFACEDESC2 desc;
    ZeroMemory(&desc, sizeof(desc));
    desc.dwSize = sizeof(desc);
    HRESULT hr = src->Lock(NULL, &desc, DDLOCK_WAIT | DDLOCK_READONLY, NULL);
    if (FAILED(hr) || !desc.lpSurface) return false;

    // 只呈現遊戲指定的 srcRect 內容區(對齊原生 primary->Blt 的 src 矩形),
    // 避免畫出 surface padding/未初始化區(在 565 下偏紅)→ 解紅框。
    int sx = 0, sy = 0, sw = (int)desc.dwWidth, sh = (int)desc.dwHeight;
    if (srcRect) {
        sx = srcRect->left;
        sy = srcRect->top;
        sw = srcRect->right  - srcRect->left;
        sh = srcRect->bottom - srcRect->top;
    }
    if (sx < 0) sx = 0;
    if (sy < 0) sy = 0;
    if (sw <= 0 || sw > (int)desc.dwWidth  - sx) sw = (int)desc.dwWidth  - sx;
    if (sh <= 0 || sh > (int)desc.dwHeight - sy) sh = (int)desc.dwHeight - sy;

    bool is565 = (*(volatile int*)gaddr::PF_SELECTOR != 0);
    const BYTE* bits = (const BYTE*)desc.lpSurface + (size_t)sy * desc.lPitch + (size_t)sx * 2;

    static volatile LONG s_logged = 0;
    if (InterlockedExchange((LONG*)&s_logged, 1) == 0)
        Log("present[1]: surf=%dx%d pitch=%ld srcRect=(%d,%d,%d,%d) is565=%d",
            (int)desc.dwWidth, (int)desc.dwHeight, desc.lPitch, sx, sy, sw, sh, (int)is565);

    PresentBits16(hwnd, bits, desc.lPitch, sw, sh, is565);
    src->Unlock(NULL);
    return true;
}

static void __cdecl PresentDetour(IDirectDrawSurface7* src, RECT* srcRect, int* dstOrigin)
{
    if (TryPresentSwapchain(src, srcRect)) return;
    ((PFN_Present)g_origPresent)(src, srcRect, dstOrigin);
}

// ───────────────────── AOB 掃描(.text)─────────────────────
//  在主模組(遊戲 exe,base 0x400000)可執行記憶體中找 pattern。回傳第一個命中位址。
static BYTE* AobScan(const BYTE* pat, size_t len)
{
    BYTE* hits[4] = {0};
    int   nhit = 0;
    SYSTEM_INFO si; GetSystemInfo(&si);
    BYTE* p   = (BYTE*)si.lpMinimumApplicationAddress;
    BYTE* end = (BYTE*)si.lpMaximumApplicationAddress;
    while (p < end) {
        MEMORY_BASIC_INFORMATION mbi;
        if (!VirtualQuery(p, &mbi, sizeof(mbi))) break;
        BYTE* base = (BYTE*)mbi.BaseAddress;
        bool exec = (mbi.State == MEM_COMMIT) &&
                    (mbi.Protect & (PAGE_EXECUTE | PAGE_EXECUTE_READ |
                                    PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY)) &&
                    !(mbi.Protect & PAGE_GUARD);
        if (exec && mbi.RegionSize >= len) {
            BYTE* rend = base + mbi.RegionSize - len;
            for (BYTE* q = base; q <= rend; ++q) {
                if (q[0] == pat[0] && memcmp(q, pat, len) == 0) {
                    if (nhit < 4) hits[nhit] = q;
                    ++nhit;
                }
            }
        }
        p = base + mbi.RegionSize;
    }
    if (nhit != 1) {
        Log("AobScan: 命中 %d 次(期望 1)", nhit);
        for (int i = 0; i < nhit && i < 4; ++i) Log("  hit[%d] = 0x%p", i, hits[i]);
    }
    if (nhit == 0) return nullptr;
    // 多命中時:優先選落在主模組 .text(< 0x00A00000,即遊戲 exe 範圍)的那個。
    HMODULE hExe = GetModuleHandleA(NULL);
    BYTE*   exeBase = (BYTE*)hExe;
    for (int i = 0; i < nhit && i < 4; ++i) {
        if (hits[i] >= exeBase && hits[i] < exeBase + 0x01879000) return hits[i];
    }
    return hits[0];
}

// ───────────────────── 內聯 hook ─────────────────────
//  覆寫 target 前 `stolen` bytes(必須是完整指令)為 jmp detour;
//  trampoline = 原 stolen bytes + jmp target+stolen,讓 detour 可回呼原函式。
static bool InstallInlineHook(BYTE* target, void* detour, void** outTramp, int stolen)
{
    if (!target) return false;
    DWORD oldProt = 0;
    if (!VirtualProtect(target, 16, PAGE_EXECUTE_READWRITE, &oldProt)) return false;

    BYTE* tramp = (BYTE*)VirtualAlloc(NULL, 64, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    if (!tramp) { VirtualProtect(target, 16, oldProt, &oldProt); return false; }
    memcpy(tramp, target, stolen);
    tramp[stolen] = 0xE9;
    *(DWORD*)(tramp + stolen + 1) =
        (DWORD)((uintptr_t)(target + stolen) - (uintptr_t)(tramp + stolen + 5));

    target[0] = 0xE9;
    *(DWORD*)(target + 1) = (DWORD)((uintptr_t)detour - (uintptr_t)(target + 5));
    for (int i = 5; i < stolen; ++i) target[i] = 0x90;   // nop 補齊

    VirtualProtect(target, 16, oldProt, &oldProt);
    FlushInstructionCache(GetCurrentProcess(), target, 16);
    *outTramp = tramp;
    return true;
}

// present prologue(0x55D460):
//   55 8B EC 83 EC 2C A1 54 3E 96 00 89 45 D4 8B 4D
//   = push ebp; mov ebp,esp; sub esp,0x2c; mov eax,[0x963e54];
//     mov [ebp-2c],eax; mov ecx,[ebp-2c]...  延長到 16 bytes 提高唯一性。
//   偷 6 bytes(55/8B EC/83 EC 2C)。
static const BYTE PRESENT_PAT[] = {
    0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x2C, 0xA1, 0x54, 0x3E, 0x96, 0x00,
    0x89, 0x45, 0xD4, 0x8B, 0x4D
};

void InstallRenderHooks()
{
    BYTE* present = AobScan(PRESENT_PAT, sizeof(PRESENT_PAT));
    if (!present) { Log("present AOB 找不到 → 放棄接管"); return; }

    g_ready = true;   // 先就緒,讓第一個 present call 就走 swapchain
    if (InstallInlineHook(present, (void*)PresentDetour, &g_origPresent, 6)) {
        Log("present hook 安裝 OK @ 0x%p tramp=0x%p", present, g_origPresent);
    } else {
        g_ready = false;
        Log("present hook 安裝失敗 @ 0x%p", present);
    }
}

// ───────────────────── worker + DllMain ─────────────────────
static DWORD WINAPI Worker(LPVOID)
{
    Log("worker 啟動,等 ddraw init...");
    // 等 primary surface 建立(ddraw init 完成 → present 函式碼已解密可 hook)
    for (int i = 0; i < 18000; ++i) {           // 180s @ 10ms
        IDirectDrawSurface7* prim = *(IDirectDrawSurface7**)gaddr::PRIMARY_SURFACE;
        if (prim) {
            // 多等一拍,確保 ddraw init 整段跑完
            Sleep(200);
            BYTE fs = *(volatile BYTE*)gaddr::FULLSCREEN_FLAG;
            Log("ddraw 就緒:primary=0x%p fullscreen=%d", prim, (int)fs);
            InstallRenderHooks();
            InstallImeRouting();
            // message loop:WinEvent OUTOFCONTEXT callback 派發到本 thread 的訊息佇列
            MSG msg;
            while (GetMessageW(&msg, NULL, 0, 0) > 0) {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            return 0;
        }
        Sleep(10);
    }
    Log("worker:等 ddraw init 逾時(180s)");
    return 0;
}

BOOL WINAPI DllMain(HINSTANCE hInst, DWORD reason, LPVOID)
{
    if (reason == DLL_PROCESS_ATTACH) {
        DisableThreadLibraryCalls(hInst);
        Log("l38ddraw.dll 載入 pid=%lu", GetCurrentProcessId());
        CreateThread(NULL, 0, Worker, NULL, 0, NULL);
    }
    return TRUE;
}
