# Build and package ClubCraft for Windows into dist\ClubCraft (exe, ONNX Runtime/DirectML DLLs).
# The separation models ship apart (tools/package-models.sh) and the app downloads them on request.
#   .\tools\package-windows.ps1               # build + package
#   .\tools\package-windows.ps1 -NoBuild      # package an existing release build
#   .\tools\package-windows.ps1 -WithModels   # also copy the ONNX models from .\models (offline package)
param([switch]$NoBuild, [switch]$WithModels)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

if (-not $NoBuild) { cargo build --release -p clubcraft }

$dist = "dist\ClubCraft"
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
New-Item -ItemType Directory -Force -Path $dist | Out-Null

Copy-Item "target\release\ClubCraft.exe" "$dist\ClubCraft.exe"
# ONNX Runtime / DirectML runtime libraries copied next to the build output by the ort crate.
Get-ChildItem "target\release" -Filter *.dll -ErrorAction SilentlyContinue | ForEach-Object { Copy-Item $_.FullName $dist }

# DirectML redistributable (ONNX Runtime 1.20 needs DirectML >= 1.15; the copy in older Windows
# builds can be too old). Official Microsoft.AI.DirectML NuGet package.
if (-not (Test-Path "$dist\DirectML.dll")) {
  $ver = "1.15.4"
  $tmp = Join-Path $env:TEMP "directml-$ver"
  if (-not (Test-Path "$tmp\bin\x64-win\DirectML.dll")) {
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    Invoke-WebRequest "https://www.nuget.org/api/v2/package/Microsoft.AI.DirectML/$ver" -OutFile "$tmp\directml.zip"
    Expand-Archive "$tmp\directml.zip" -DestinationPath $tmp -Force
  }
  Copy-Item "$tmp\bin\x64-win\DirectML.dll" $dist
  Copy-Item "$tmp\LICENSE.txt" "$dist\DirectML-LICENSE.txt" -ErrorAction SilentlyContinue
}

# Models (offline package only): ONNX files + manifests (CoreML packages are macOS-only).
if ($WithModels -and (Test-Path "models")) {
  New-Item -ItemType Directory -Force -Path "$dist\models" | Out-Null
  Copy-Item "models\README.md" "$dist\models\README.md" -ErrorAction SilentlyContinue
  Get-ChildItem "models" -Include *.json, *.onnx, *.onnx.data, cocola_w.bin -File -Recurse | Where-Object { $_.Name -notlike "*_ref_*" -and $_.Name -notlike "htdemucs_6s*" } | ForEach-Object { Copy-Item $_.FullName "$dist\models" }
}
Copy-Item "README.md" $dist

if ($WithModels) {
  $n = (Get-ChildItem "$dist\models" -Filter *.onnx -ErrorAction SilentlyContinue).Count
  Write-Host "Packaged $dist ($n ONNX models)."
} else {
  Write-Host "Packaged $dist without models (Settings > Analysis & stems > Download models)."
}
