; PS7 ISE installer
; Build with: makensis PS7ISE-Installer.nsi
; Target: C:\Program Files\HCW\PS7ISE

; General settings
Name "PS7 ISE"
OutFile "PS7ISE-Setup.exe"
InstallDir "$PROGRAMFILES64\HCW\PS7ISE"
RequestExecutionLevel admin

; Modern UI
!include "MUI2.nsh"
!include "LogicLib.nsh"

; The folder to package. makensis /DPAYLOAD=dist-setup overrides it.
!ifndef PAYLOAD
  !define PAYLOAD "dist"
!endif

; Version that the front-end already carries in Cargo.toml (and in the exe's version
; resource through build.rs). Added to Add/Remove Programs so a user or tool can see
; which build is installed without opening the exe's Properties sheet. The number must
; stay in sync with the front end; a mismatch here is cosmetic only and never breaks
; install or uninstall.
!ifndef VERSION
  !define VERSION "1.4.0"
!endif

; The installer's own version resource, so Explorer's Properties sheet on PS7ISE-Setup.exe says
; the same number as the title bar of the program inside it rather than nothing at all. NSIS
; insists on four numbers for VIProductVersion, unlike the three the front end carries, so the
; fourth is written down rather than guessed at by the tool.
!define FILEVERSION "${VERSION}.0"
VIProductVersion "${FILEVERSION}"
VIAddVersionKey /LANG=1033 "CompanyName" "HCW"
VIAddVersionKey /LANG=1033 "FileDescription" "PS7 ISE installer"
VIAddVersionKey /LANG=1033 "FileVersion" "${FILEVERSION}"
VIAddVersionKey /LANG=1033 "ProductName" "PS7 ISE"
VIAddVersionKey /LANG=1033 "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=1033 "LegalCopyright" "MIT"

; Interface
!define MUI_ABORTWARNING
!define MUI_ICON "assets\ps7ise.ico"
!define MUI_UNICON "assets\ps7ise.ico"

; Pages
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; Language
!insertmacro MUI_LANGUAGE "English"

; Main section
Section "Install"
    SetOutPath $INSTDIR

    ; Copy the whole payload: the front-end, the engine, the PowerShell assemblies, the
    ; modules, and - in the build this installer ships - the .NET runtime itself.
    ;
    ; The payload defaults to dist but is overridable so the release can package a
    ; self-contained build without turning the repository's dist into one:
    ;   makensis /DPAYLOAD=dist-setup PS7ISE-Installer.nsi
    ;
    ; Nothing is filtered out. hostfxr.dll and hostpolicy.dll stay in: a self-contained
    ; payload needs them, and build.ps1 only strips them from a framework-dependent one.
    File /r "${PAYLOAD}\*"

    ; Create a start menu shortcut
    CreateDirectory "$SMPROGRAMS\HCW"
    CreateShortCut "$SMPROGRAMS\HCW\PS7 ISE.lnk" "$INSTDIR\PS7_ISE.exe"

    ; Create a desktop shortcut (optional)
    CreateShortCut "$DESKTOP\PS7 ISE.lnk" "$INSTDIR\PS7_ISE.exe"

    ; Add to PATH so PS7_ISE.exe can be run from anywhere
    ; Only if the user wants it — not adding by default.
    ; Uncomment the lines below if you want PATH to be updated automatically.
    ; Env::AddValue /APPEND "PATH" "$INSTDIR"
    ; Env::AddValue /APPEND "PATH" "$INSTDIR" /MACHINE

    ; Write the install location to the registry for uninstall
    ;
    ; SetRegView 64 because makensis is itself a 32-bit program: left alone, NSIS writes into
    ; HKLM\Software\WOW6432Node, where Add/Remove Programs still finds the entry but a 64-bit
    ; tool enumerating the real Uninstall key does not.
    SetRegView 64

    ; An install by an earlier build of this script left the entry in the 32-bit view; drop it
    ; on upgrade so the same name does not appear twice.
    SetRegView 32
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\PS7ISE"
    SetRegView 64

    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\PS7ISE" \
        "DisplayName" "PS7 ISE"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\PS7ISE" \
        "UninstallString" '"$INSTDIR\uninstall.exe"'
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\PS7ISE" \
        "InstallLocation" "$INSTDIR"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\PS7ISE" \
        "Publisher" "HCW"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\PS7ISE" \
        "DisplayVersion" "${VERSION}"
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\PS7ISE" \
        "NoModify" 1
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\PS7ISE" \
        "NoRepair" 1

    ; Generate the uninstaller
    WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd

; Uninstall section
Section "Uninstall"
    ; Remove the whole folder and everything in it.
    RMDir /r "$INSTDIR"

    ; Remove shortcuts
    Delete "$SMPROGRAMS\HCW\PS7 ISE.lnk"
    RMDir "$SMPROGRAMS\HCW"
    Delete "$DESKTOP\PS7 ISE.lnk"

    ; Remove registry entries. Both views: an install by an older build put the entry in
    ; WOW6432Node, a current one puts it in the native 64-bit key, and whichever this machine
    ; has must go, or the app stays listed in Add/Remove Programs after uninstalling.
    SetRegView 64
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\PS7ISE"
    SetRegView 32
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\PS7ISE"

    ; Remove from PATH if we added it (commented out above, so this is safe)
    ; Env::DeleteValue "PATH" "$INSTDIR"
SectionEnd
