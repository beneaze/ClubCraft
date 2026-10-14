# Build and package ClubCraft for Windows into dist\ClubCraft (exe, ONNX Runtime/DirectML DLLs,
# and the ONNX separation models if they exist in .\models).
#   .\tools\package-windows.ps1            # build + package
#   .\tools\package-windows.ps1 -NoBuild   # package an existing release build
param([switch]$NoBuild)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

if (-not $NoBuild) { cargo build --release -p clubcraft }

$dist = "dist\ClubCraft"
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
New-Item -ItemType Directory -Force -Path "$dist\models" | Out-Null

Copy-Item "target\release\clubcraft.exe" "$dist\ClubCraft.exe"
# ONNX Runtime / DirectML runtime libraries copied next to the build output by the ort crate.
Get-ChildItem "target\release" -Filter *.dll -ErrorAction SilentlyContinue | ForEach-Object { Copy-Item $_.FullName $dist }

# Models: ONNX files + manifests (CoreML packages are macOS-only).
if (Test-Path "models") {
  Get-ChildItem "models" -Include *.json, *.onnx, *.onnx.data -File -Recurse | Where-Object { $_.Name -notlike "*_ref_*" } | ForEach-Object { Copy-Item $_.FullName "$dist\models" }
}
Copy-Item "README.md" $dist
Copy-Item "models\README.md" "$dist\models\README.md" -ErrorAction SilentlyContinue

$n = (Get-ChildItem "$dist\models" -Filter *.onnx -ErrorAction SilentlyContinue).Count
Write-Host "Packaged $dist ($n ONNX models). Without models, ClubCraft runs but cannot separate stems; see models\README.md."
