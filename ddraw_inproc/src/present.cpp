// present.cpp —— D3D11 + DXGI swapchain present(取代原生 primary->Blt-to-window）
//
// 根本病灶:遊戲假設 primary = 桌面 front buffer,寫 primary 即上螢幕。
// Win11 DWM 把桌面虛擬化,原生視窗化 primary->Blt 雖能顯示主場景,卻會每幀蓋過
// 輸入框 child 視窗區 → 黑底 + 打字閃爍。
//
// 解法(對準 dgVoodoo 已驗證內核,自製):在遊戲 HWND 上建 DXGI swapchain,
//   - BitBlt-model(DXGI_SWAP_EFFECT_DISCARD)+ GDI_COMPATIBLE → backbuffer 可 GetDC,
//     且 DWM 會把 GDI 子視窗(輸入框 LUnicodeEdit)+ 系統 IME 候選視窗正確疊上。
//   - 每幀:Lock 來源 surface 拿 565/555 像素 → StretchDIBits 到 backbuffer DC
//     → ReleaseDC → Present。Present 原子交換整張 backbuffer → 不閃。
//
// 全部繁體中文註解(專案規則)。
#include "inhook.h"
#include <d3d11.h>
#include <dxgi.h>

// ───────────────────────── D3D 全域狀態 ─────────────────────────
static ID3D11Device*        g_d3dDev    = nullptr;
static ID3D11DeviceContext* g_d3dCtx    = nullptr;
static IDXGISwapChain*      g_swap      = nullptr;
static HWND                 g_swapHwnd  = nullptr;
static int                  g_swapW     = 0;
static int                  g_swapH     = 0;
static bool                 g_d3dFailed = false;   // 初始化失敗即放棄,避免每幀重試刷 log

static void ReleaseSwap()
{
    if (g_swap) { g_swap->Release(); g_swap = nullptr; }
    g_swapHwnd = nullptr; g_swapW = 0; g_swapH = 0;
}

// 建立 / 重建 device + swapchain,尺寸對齊視窗 client。回傳是否就緒。
static bool EnsureD3D(HWND hwnd, int cw, int ch)
{
    if (g_d3dFailed) return false;
    if (g_d3dDev && g_swap && g_swapHwnd == hwnd && g_swapW == cw && g_swapH == ch)
        return true;

    // device 只建一次(硬體優先,失敗退 WARP 軟體保命)
    if (!g_d3dDev) {
        D3D_FEATURE_LEVEL fl;
        HRESULT hr = D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr,
            0, nullptr, 0, D3D11_SDK_VERSION, &g_d3dDev, &fl, &g_d3dCtx);
        if (FAILED(hr)) {
            hr = D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_WARP, nullptr,
                0, nullptr, 0, D3D11_SDK_VERSION, &g_d3dDev, &fl, &g_d3dCtx);
        }
        if (FAILED(hr)) {
            Log("present: D3D11CreateDevice 失敗 hr=0x%08lX", hr);
            g_d3dFailed = true;
            return false;
        }
    }

    // 由 device 反查 DXGI factory(不用 CreateDXGIFactory,避免 adapter 不一致)
    ReleaseSwap();
    IDXGIDevice*  dxgiDev = nullptr;
    IDXGIAdapter* adapter = nullptr;
    IDXGIFactory* factory = nullptr;
    if (FAILED(g_d3dDev->QueryInterface(__uuidof(IDXGIDevice), (void**)&dxgiDev)) ||
        FAILED(dxgiDev->GetAdapter(&adapter)) ||
        FAILED(adapter->GetParent(__uuidof(IDXGIFactory), (void**)&factory))) {
        if (adapter) adapter->Release();
        if (dxgiDev) dxgiDev->Release();
        Log("present: 取得 DXGIFactory 失敗");
        g_d3dFailed = true;
        return false;
    }

    DXGI_SWAP_CHAIN_DESC sd;
    ZeroMemory(&sd, sizeof(sd));
    sd.BufferCount        = 1;
    sd.BufferDesc.Width   = cw;
    sd.BufferDesc.Height  = ch;
    sd.BufferDesc.Format  = DXGI_FORMAT_B8G8R8A8_UNORM;   // GetDC 要求此格式
    sd.BufferUsage        = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.SampleDesc.Count   = 1;
    sd.OutputWindow       = hwnd;
    sd.Windowed           = TRUE;
    sd.SwapEffect         = DXGI_SWAP_EFFECT_DISCARD;        // BitBlt-model:支援 GetDC + DWM 疊 GDI child
    sd.Flags              = DXGI_SWAP_CHAIN_FLAG_GDI_COMPATIBLE;

    HRESULT hr = factory->CreateSwapChain(g_d3dDev, &sd, &g_swap);
    // 不讓 DXGI 攔 Alt+Enter / 改視窗(我們一律視窗化)
    factory->MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_WINDOW_CHANGES);
    factory->Release();
    adapter->Release();
    dxgiDev->Release();

    if (FAILED(hr)) {
        Log("present: CreateSwapChain 失敗 hr=0x%08lX cw=%d ch=%d", hr, cw, ch);
        g_d3dFailed = true;
        return false;
    }

    g_swapHwnd = hwnd; g_swapW = cw; g_swapH = ch;
    Log("present: swapchain OK hwnd=%p client=%dx%d", hwnd, cw, ch);
    return true;
}

// 建一個 16bpp(565 或 555)的 BI_BITFIELDS BITMAPINFO。
//   biWidth  = pitch/2(以 pixel 計,讓 row stride == lPitch),src 取左側 sw pixel。
//   biHeight = -sh(top-down,對齊 DDraw surface row0=top)。
struct Bmi565 { BITMAPINFOHEADER h; DWORD mask[3]; };
static void FillBmi(Bmi565* b, int pitch, int sh, bool is565)
{
    ZeroMemory(b, sizeof(*b));
    b->h.biSize        = sizeof(BITMAPINFOHEADER);
    b->h.biWidth       = pitch / 2;
    b->h.biHeight      = -sh;              // top-down
    b->h.biPlanes      = 1;
    b->h.biBitCount    = 16;
    b->h.biCompression = BI_BITFIELDS;
    if (is565) { b->mask[0] = 0xF800; b->mask[1] = 0x07E0; b->mask[2] = 0x001F; }
    else       { b->mask[0] = 0x7C00; b->mask[1] = 0x03E0; b->mask[2] = 0x001F; }
}

void PresentBits16(HWND hwnd, const void* bits, int srcPitch, int sw, int sh, bool is565)
{
    if (!hwnd || !bits || srcPitch <= 0 || sw <= 0 || sh <= 0) return;
    RECT rc;
    if (!GetClientRect(hwnd, &rc)) return;
    int cw = rc.right - rc.left, ch = rc.bottom - rc.top;
    if (cw <= 0 || ch <= 0) return;
    if (!EnsureD3D(hwnd, cw, ch)) return;

    IDXGISurface1* surf = nullptr;
    if (FAILED(g_swap->GetBuffer(0, __uuidof(IDXGISurface1), (void**)&surf)) || !surf) return;

    HDC dstDC = nullptr;
    if (SUCCEEDED(surf->GetDC(FALSE, &dstDC)) && dstDC) {
        // DISCARD swapchain present 後 backbuffer 內容未定義 → 先清黑,避免沒覆蓋到的
        // 區域殘留舊幀(捲動時成拖尾)。若 StretchDIBits 全覆蓋則此步無視覺影響。
        PatBlt(dstDC, 0, 0, cw, ch, BLACKNESS);
        Bmi565 bmi;
        FillBmi(&bmi, srcPitch, sh, is565);
        SetStretchBltMode(dstDC, COLORONCOLOR);
        StretchDIBits(dstDC, 0, 0, cw, ch, 0, 0, sw, sh,
                      bits, (BITMAPINFO*)&bmi, DIB_RGB_COLORS, SRCCOPY);
        RECT dirty = { 0, 0, cw, ch };
        surf->ReleaseDC(&dirty);   // 必須 ReleaseDC 後才能 Present
    }
    surf->Release();

    // Present:整張 backbuffer 原子交換 → 不閃。視窗化 swapchain 由 DWM 統一合成。
    // SyncInterval=0 不自等 vblank,避免一幀多次 present 時 vsync 多次 stall 拉低幀率。
    HRESULT hr = g_swap->Present(0, 0);
    if (hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET) {
        Log("present: 裝置遺失 hr=0x%08lX → 下幀重建", hr);
        ReleaseSwap();
        if (g_d3dCtx) { g_d3dCtx->Release(); g_d3dCtx = nullptr; }
        if (g_d3dDev) { g_d3dDev->Release(); g_d3dDev = nullptr; }
    }
}
