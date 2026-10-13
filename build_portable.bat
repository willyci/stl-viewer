@echo off
echo ==============================================
echo Building STL Viewer - Portable Version
echo ==============================================

:: 1. Ensure output folder exists or run build
echo Running PyInstaller...
pyinstaller app.spec

:: 2. Create portable.txt inside the compiled output directory
echo Creating portable.txt marker file in output...
echo # Portable Mode Marker > dist\STLViewer\portable.txt

echo ==============================================
echo Build Completed successfully!
echo Portable version is located in: dist\STLViewer\
echo You can now compress/zip this folder for distribution.
echo ==============================================
