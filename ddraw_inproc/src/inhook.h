// l38ddraw.dll —— 注入式 in-process DDraw present 接管(ALW 路線,非 ddraw.dll 代理)
//
// 目標:把遊戲視窗化的最終 present 從原生 `primary->Blt(視窗矩形, stretch)` 改走
//   我們在遊戲 HWND 上自建的 DXGI swapchain(BitBlt-model + GDI_COMPATIBLE)。
//   → DWM 統一合成:不黑、不撕裂、原子交換不閃,且子視窗(輸入框 LUnicodeEdit)
//     + 系統 IME 候選視窗會被 DWM 正確疊在 swapchain 上方。
//
// 全部繁體中文註解(專案規則)。
#pragma once
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <ddraw.h>

// ───────────────────────── 記錄器(log.cpp)─────────────────────────
void Log(const char* fmt, ...);

// ───────────────────── 遊戲全域(.data,packer 解後位址穩定)─────────────────────
//  注意:code hook 點走 AOB(live build 可能 +0x390 偏移),但這些 .data 全域位址穩定。
namespace gaddr {
    constexpr uintptr_t FULLSCREEN_FLAG = 0x009a84d0; // byte:0=視窗(DDSCL_NORMAL),非0=全螢幕
    constexpr uintptr_t PF_SELECTOR     = 0x009a235c; // int:0=RGB555,1=RGB565
    constexpr uintptr_t PRIMARY_SURFACE = 0x009a84e8; // IDirectDrawSurface7* primary
}

// ───────────────────────── present.cpp ─────────────────────────
//  把一塊 16bpp(565/555)像素(top-down)StretchDIBits 到 DXGI swapchain backbuffer 再 Present。
//   bits     = Lock 拿到的來源像素
//   srcPitch = Lock 回傳的 lPitch(byte/列)
//   sw,sh    = 來源有效寬高(stretch 通常 800x600)
//   is565    = true:RGB565,false:RGB555
void PresentBits16(HWND hwnd, const void* bits, int srcPitch, int sw, int sh, bool is565);

// ───────────────────────── main.cpp ─────────────────────────
void InstallRenderHooks();

// ───────────────────────── ime.cpp ─────────────────────────
//  原生 IME 候選視窗路由:subclass LUnicodeEdit,把純通知類 IME 訊息先轉 DefWindowProc
//  讓 IMM32 開原生候選視窗(DWM 會合成在 swapchain 上方)。無自繪。
//  須在有 message loop 的 thread 呼叫(WinEvent OUTOFCONTEXT callback 用)。
void InstallImeRouting();
