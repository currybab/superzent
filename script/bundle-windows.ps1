[CmdletBinding()]
Param(
    [Parameter()][Alias('i')][switch]$Install,
    [Parameter()][Alias('h')][switch]$Help,
    [Parameter()][Alias('a')][string]$Architecture,
    [Parameter()][string]$Name
)

. "$PSScriptRoot/lib/workspace.ps1"

# https://stackoverflow.com/questions/57949031/powershell-script-stops-if-program-fails-like-bash-set-o-errexit
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$buildSuccess = $false

$OSArchitecture = switch ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture) {
    "X64" { "x86_64" }
    "Arm64" { "aarch64" }
    default { throw "Unsupported architecture" }
}

$Architecture = if ($Architecture) {
    $Architecture
} else {
    $OSArchitecture
}

$CargoOutDir = "./target/$Architecture-pc-windows-msvc/release"

function Get-VSArch {
    param(
        [string]$Arch
    )

    switch ($Arch) {
        "x86_64" { "amd64" }
        "aarch64" { "arm64" }
    }
}

Push-Location
& "C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\Tools\Launch-VsDevShell.ps1" -Arch (Get-VSArch -Arch $Architecture) -HostArch (Get-VSArch -Arch $OSArchitecture)
Pop-Location

$target = "$Architecture-pc-windows-msvc"

if ($Help) {
    Write-Output "Usage: test.ps1 [-Install] [-Help]"
    Write-Output "Build the installer for Windows.\n"
    Write-Output "Options:"
    Write-Output "  -Architecture, -a Which architecture to build (x86_64 or aarch64)"
    Write-Output "  -Install, -i      Run the installer after building."
    Write-Output "  -Help, -h         Show this help message."
    exit 0
}

Push-Location -Path crates/zed
$channel = Get-Content "RELEASE_CHANNEL"
$env:ZED_RELEASE_CHANNEL = $channel
$env:RELEASE_CHANNEL = $channel
Pop-Location

function CheckEnvironmentVariables {
    if(-not $env:CI) {
        return
    }

    $requiredVars = @(
        'ZED_WORKSPACE', 'RELEASE_VERSION', 'ZED_RELEASE_CHANNEL',
        'AZURE_TENANT_ID', 'AZURE_CLIENT_ID', 'AZURE_CLIENT_SECRET',
        'ACCOUNT_NAME', 'CERT_PROFILE_NAME', 'ENDPOINT',
        'FILE_DIGEST', 'TIMESTAMP_DIGEST', 'TIMESTAMP_SERVER'
    )

    foreach ($var in $requiredVars) {
        if (-not (Test-Path "env:$var")) {
            Write-Error "$var is not set"
            exit 1
        }
    }
}

function PrepareForBundle {
    if (Test-Path "$innoDir") {
        Remove-Item -Path "$innoDir" -Recurse -Force
    }
    New-Item -Path "$innoDir" -ItemType Directory -Force
    Copy-Item -Path "$env:ZED_WORKSPACE\crates\zed\resources\windows\*" -Destination "$innoDir" -Recurse -Force
    New-Item -Path "$innoDir\make_appx" -ItemType Directory -Force
    New-Item -Path "$innoDir\appx" -ItemType Directory -Force
    New-Item -Path "$innoDir\bin" -ItemType Directory -Force
    New-Item -Path "$innoDir\tools" -ItemType Directory -Force

    rustup target add $target
}

function GenerateLicenses {
    . $PSScriptRoot/generate-licenses.ps1
}

function BuildZedAndItsFriends {
    Write-Output "Building superzent and its friends, for channel: $channel"
    # Build superzent.exe, cli.exe and auto_update_helper.exe
    cargo build --release --package superzent --package cli --package auto_update_helper --target $target
    Copy-Item -Path ".\$CargoOutDir\superzent.exe" -Destination "$innoDir\superzent.exe" -Force
    Copy-Item -Path ".\$CargoOutDir\cli.exe" -Destination "$innoDir\cli.exe" -Force
    Copy-Item -Path ".\$CargoOutDir\auto_update_helper.exe" -Destination "$innoDir\auto_update_helper.exe" -Force
    # Build explorer_command_injector.dll
    switch ($channel) {
        "stable" {
            cargo build --release --features stable --no-default-features --package explorer_command_injector --target $target
        }
        "dev" {
            cargo build --release --features dev --no-default-features --package explorer_command_injector --target $target
        }
        default {
            throw "Unsupported release channel: $channel"
        }
    }
    Copy-Item -Path ".\$CargoOutDir\explorer_command_injector.dll" -Destination "$innoDir\zed_explorer_command_injector.dll" -Force
}

function BuildRemoteServer {
    Write-Output "Building remote_server for $target"
    cargo build --release --package remote_server --target $target

    # Create zipped remote server binary
    $remoteServerSrc = (Resolve-Path ".\$CargoOutDir\remote_server.exe").Path

    if ($env:CI) {
        Write-Output "Code signing remote_server.exe"
        & "$innoDir\sign.ps1" $remoteServerSrc
    }

    $remoteServerDst = "$env:ZED_WORKSPACE\target\superzent-remote-server-windows-$Architecture.zip"
    Write-Output "Compressing remote_server to $remoteServerDst"
    Compress-Archive -Path $remoteServerSrc -DestinationPath $remoteServerDst -Force

    Write-Output "Remote server compressed successfully"
}

function ZipZedAndItsFriendsDebug {
    $items = @(
        ".\$CargoOutDir\superzent.pdb",
        ".\$CargoOutDir\cli.pdb",
        ".\$CargoOutDir\auto_update_helper.pdb",
        ".\$CargoOutDir\explorer_command_injector.pdb",
        ".\$CargoOutDir\remote_server.pdb"
    )

    Compress-Archive -Path $items -DestinationPath ".\$CargoOutDir\superzent-$env:RELEASE_VERSION-$env:ZED_RELEASE_CHANNEL.dbg.zip" -Force
}


function UploadToSentry {
    if (-not (Get-Command "sentry-cli" -ErrorAction SilentlyContinue)) {
        Write-Output "sentry-cli not found. skipping sentry upload."
        Write-Output "install with: 'winget install -e --id=Sentry.sentry-cli'"
        return
    }
    if (-not (Test-Path "env:SENTRY_AUTH_TOKEN")) {
        Write-Output "missing SENTRY_AUTH_TOKEN. skipping sentry upload."
        return
    }
    Write-Output "Uploading superzent debug symbols to sentry..."
    for ($i = 1; $i -le 3; $i++) {
        try {
            sentry-cli debug-files upload --include-sources --wait -p superzent -o superzent $CargoOutDir
            break
        }
        catch {
            Write-Output "Sentry upload attempt $i failed: $_"
            if ($i -eq 3) {
                Write-Output "All sentry upload attempts failed"
                throw
            }
            Start-Sleep -Seconds 2
        }
    }
}

function MakeAppx {
    switch ($channel) {
        "stable" {
            $manifestFile = "$env:ZED_WORKSPACE\crates\explorer_command_injector\AppxManifest.xml"
        }
        "dev" {
            $manifestFile = "$env:ZED_WORKSPACE\crates\explorer_command_injector\AppxManifest-Dev.xml"
        }
        default {
            throw "Unsupported release channel: $channel"
        }
    }
    Copy-Item -Path "$manifestFile" -Destination "$innoDir\make_appx\AppxManifest.xml"
    # Add makeAppx.exe to Path
    $sdk = "C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64"
    $env:Path += ';' + $sdk
    makeAppx.exe pack /d "$innoDir\make_appx" /p "$innoDir\zed_explorer_command_injector.appx" /nv
}

function SignZedAndItsFriends {
    if (-not $env:CI) {
        return
    }

    $files = "$innoDir\superzent.exe,$innoDir\cli.exe,$innoDir\auto_update_helper.exe,$innoDir\zed_explorer_command_injector.dll,$innoDir\zed_explorer_command_injector.appx"
    & "$innoDir\sign.ps1" $files
}

function DownloadAMDGpuServices {
    # If you update the AGS SDK version, please also update the version in `crates/gpui/src/platform/windows/directx_renderer.rs`
    $url = "https://codeload.github.com/GPUOpen-LibrariesAndSDKs/AGS_SDK/zip/refs/tags/v6.3.0"
    $zipPath = ".\AGS_SDK_v6.3.0.zip"
    # Download the AGS SDK zip file
    Invoke-WebRequest -Uri $url -OutFile $zipPath
    # Extract the AGS SDK zip file
    Expand-Archive -Path $zipPath -DestinationPath "." -Force
}

function DownloadConpty {
    $url = "https://github.com/microsoft/terminal/releases/download/v1.23.13503.0/Microsoft.Windows.Console.ConPTY.1.23.251216003.nupkg"
    $zipPath = ".\Microsoft.Windows.Console.ConPTY.1.23.251216003.nupkg"
    Invoke-WebRequest -Uri $url -OutFile $zipPath
    Expand-Archive -Path $zipPath -DestinationPath ".\conpty" -Force
}

function CollectFiles {
    Move-Item -Path "$innoDir\zed_explorer_command_injector.appx" -Destination "$innoDir\appx\zed_explorer_command_injector.appx" -Force
    Move-Item -Path "$innoDir\zed_explorer_command_injector.dll" -Destination "$innoDir\appx\zed_explorer_command_injector.dll" -Force
    Move-Item -Path "$innoDir\cli.exe" -Destination "$innoDir\bin\superzent.exe" -Force
    Move-Item -Path "$innoDir\zed.sh" -Destination "$innoDir\bin\superzent" -Force
    Move-Item -Path "$innoDir\auto_update_helper.exe" -Destination "$innoDir\tools\auto_update_helper.exe" -Force
    if($Architecture -eq "aarch64") {
        New-Item -Type Directory -Path "$innoDir\arm64" -Force
        Move-Item -Path ".\conpty\build\native\runtimes\arm64\OpenConsole.exe" -Destination "$innoDir\arm64\OpenConsole.exe" -Force
        Move-Item -Path ".\conpty\runtimes\win-arm64\native\conpty.dll" -Destination "$innoDir\conpty.dll" -Force
    }
    else {
        New-Item -Type Directory -Path "$innoDir\x64" -Force
        New-Item -Type Directory -Path "$innoDir\arm64" -Force
        Move-Item -Path ".\AGS_SDK-6.3.0\ags_lib\lib\amd_ags_x64.dll" -Destination "$innoDir\amd_ags_x64.dll" -Force
        Move-Item -Path ".\conpty\build\native\runtimes\x64\OpenConsole.exe" -Destination "$innoDir\x64\OpenConsole.exe" -Force
        Move-Item -Path ".\conpty\build\native\runtimes\arm64\OpenConsole.exe" -Destination "$innoDir\arm64\OpenConsole.exe" -Force
        Move-Item -Path ".\conpty\runtimes\win-x64\native\conpty.dll" -Destination "$innoDir\conpty.dll" -Force
    }
}

function BuildInstaller {
    $issFilePath = "$innoDir\zed.iss"
    switch ($channel) {
        "stable" {
            $appId = "{{2DB0DA96-CA55-49BB-AF4F-64AF36A86712}"
            $appIconName = "app-icon-nightly"
            $appName = "superzent"
            $appDisplayName = "superzent"
            $appSetupName = "superzent-$Architecture"
            # The mutex name here should match the mutex name in crates\zed\src\zed\windows_only_instance.rs
            $appMutex = "superzent-stable-Instance-Mutex"
            $appExeName = "superzent"
            $regValueName = "Superzent"
            $appUserId = "Nangman.Superzent"
            $appShellNameShort = "s&uperzet"
            $appAppxFullName = "Nangman.Superzent_1.0.0.0_neutral__japxn1gcva8rg"
        }
        "dev" {
            $appId = "{{8357632E-24A4-4F32-BA97-E575B4D1FE5D}"
            $appIconName = "app-icon"
            $appName = "superzent dev"
            $appDisplayName = "superzent dev"
            $appSetupName = "superzent-$Architecture"
            # The mutex name here should match the mutex name in crates\zed\src\zed\windows_only_instance.rs
            $appMutex = "superzent-dev-Instance-Mutex"
            $appExeName = "superzent"
            $regValueName = "SuperzentDev"
            $appUserId = "Nangman.Superzent.Dev"
            $appShellNameShort = "superzent &dev"
            $appAppxFullName = "Nangman.Superzent.Dev_1.0.0.0_neutral__japxn1gcva8rg"
        }
        default {
            Write-Error "can't bundle installer for $channel."
            exit 1
        }
    }

    # Windows runner 2022 default has iscc in PATH, https://github.com/actions/runner-images/blob/main/images/windows/Windows2022-Readme.md
    # Currently, we are using Windows 2022 runner.
    # Windows runner 2025 doesn't have iscc in PATH for now, https://github.com/actions/runner-images/issues/11228
    $innoSetupPath = "C:\Program Files (x86)\Inno Setup 6\ISCC.exe"

    $definitions = @{
        "AppId"          = $appId
        "AppIconName"    = $appIconName
        "OutputDir"      = "$env:ZED_WORKSPACE\target"
        "AppSetupName"   = $appSetupName
        "AppName"        = $appName
        "AppDisplayName" = $appDisplayName
        "RegValueName"   = $regValueName
        "AppMutex"       = $appMutex
        "AppExeName"     = $appExeName
        "ResourcesDir"   = "$innoDir"
        "ShellNameShort" = $appShellNameShort
        "AppUserId"      = $appUserId
        "Version"        = "$env:RELEASE_VERSION"
        "SourceDir"      = "$env:ZED_WORKSPACE"
        "AppxFullName"   = $appAppxFullName
    }

    $defs = @()
    foreach ($key in $definitions.Keys) {
        $defs += "/d$key=`"$($definitions[$key])`""
    }

    $innoArgs = @($issFilePath) + $defs
    if($env:CI) {
        $signTool = "powershell.exe -ExecutionPolicy Bypass -File $innoDir\sign.ps1 `$f"
        $innoArgs += "/sDefaultsign=`"$signTool`""
    }

    # Execute Inno Setup
    Write-Host "🚀 Running Inno Setup: $innoSetupPath $innoArgs"
    $process = Start-Process -FilePath $innoSetupPath -ArgumentList $innoArgs -NoNewWindow -Wait -PassThru

    if ($process.ExitCode -eq 0) {
        Write-Host "✅ Inno Setup successfully compiled the installer"
        Write-Output "SETUP_PATH=target/$appSetupName.exe" >> $env:GITHUB_ENV
        $script:buildSuccess = $true
    }
    else {
        Write-Host "❌ Inno Setup failed: $($process.ExitCode)"
        $script:buildSuccess = $false
    }
}

ParseZedWorkspace
$innoDir = "$env:ZED_WORKSPACE\inno\$Architecture"
$debugArchive = "$CargoOutDir\superzent-$env:RELEASE_VERSION-$env:ZED_RELEASE_CHANNEL.dbg.zip"
$debugStoreKey = "$env:ZED_RELEASE_CHANNEL/superzent-$env:RELEASE_VERSION-$env:ZED_RELEASE_CHANNEL.dbg.zip"

CheckEnvironmentVariables
PrepareForBundle
GenerateLicenses
BuildZedAndItsFriends
BuildRemoteServer
MakeAppx
SignZedAndItsFriends
ZipZedAndItsFriendsDebug
DownloadAMDGpuServices
DownloadConpty
CollectFiles
BuildInstaller

if($env:CI) {
    UploadToSentry
}

if ($buildSuccess) {
    Write-Output "Build successful"
    if ($Install) {
        Write-Output "Installing superzent..."
        Start-Process -FilePath "$env:ZED_WORKSPACE/target/superzent-$Architecture.exe"
    }
    exit 0
}
else {
    Write-Output "Build failed"
    exit 1
}
