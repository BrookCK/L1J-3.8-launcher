@echo off
REM l38ddraw.dll - injected in-process DDraw present takeover (32-bit / x86)
setlocal
pushd "%~dp0"

set "VCVARS=C:\Program Files\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars32.bat"
if not exist "%VCVARS%" (
    echo [ERROR] vcvars32.bat not found: %VCVARS%
    popd & exit /b 1
)
call "%VCVARS%"

if not exist build mkdir build

REM /LD=DLL  /MT=static CRT  /O2  /EHsc
cl /nologo /utf-8 /LD /MT /O2 /EHsc /W3 /DWIN32 /D_WINDOWS ^
   src\log.cpp src\present.cpp src\ime.cpp src\main.cpp ^
   /Fobuild\ /Febuild\l38ddraw.dll ^
   /link user32.lib gdi32.lib imm32.lib ddraw.lib dxguid.lib d3d11.lib dxgi.lib

set RC=%ERRORLEVEL%
echo.
if "%RC%"=="0" (
    echo [OK] built build\l38ddraw.dll
) else (
    echo [FAIL] cl returned %RC%
)
popd
endlocal & exit /b %RC%
