# -*- mode: python ; coding: utf-8 -*-
#
# PyInstaller spec for STL Viewer (PySide6 + VTK)
# Build:  pyinstaller app.spec
# Output: dist\STLViewer.exe

from PyInstaller.utils.hooks import collect_all

# ── VTK: collect everything ───────────────────────────────────────────────────
vtk_datas, vtk_binaries, vtk_hiddenimports = collect_all('vtkmodules')

# ── NumPy ─────────────────────────────────────────────────────────────────────
np_datas, np_binaries, np_hiddenimports = collect_all('numpy')

a = Analysis(
    ['app.py'],
    pathex=['.'],
    binaries=vtk_binaries + np_binaries,
    datas=vtk_datas + np_datas + [
        ('parsers.py',  '.'),
        ('renderer.py', '.'),
        ('styles.py',   '.'),
    ],
    hiddenimports=vtk_hiddenimports + np_hiddenimports + [
        'vtkmodules.util.numpy_support',
        'vtkmodules.util.vtkAlgorithm',
        'PySide6.QtSvgWidgets',
        'PySide6.QtMultimedia',
        'PySide6.QtMultimediaWidgets',
        'PySide6.QtOpenGL',
        'PySide6.QtOpenGLWidgets',
        'sqlite3',
        'zipfile',
        'struct',
        'hashlib',
        'math',
    ],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[
        # Unused Python packages
        'matplotlib',
        'scipy',
        'IPython',
        'PIL',
        'cv2',
        'PyQt5',
        'PyQt6',
        'tkinter',

        # Large optional Qt modules not used by this app
        # (QtNetwork is intentionally kept — shiboken requires it)
        'PySide6.Qt3DCore',
        'PySide6.Qt3DRender',
        'PySide6.Qt3DInput',
        'PySide6.Qt3DAnimation',
        'PySide6.Qt3DExtras',
        'PySide6.QtQuick',
        'PySide6.QtQml',
        'PySide6.QtWebEngine',
        'PySide6.QtWebEngineCore',
        'PySide6.QtWebEngineWidgets',
        'PySide6.QtCharts',
        'PySide6.QtDataVisualization',
        'PySide6.QtBluetooth',
        'PySide6.QtNfc',
        'PySide6.QtPositioning',
        'PySide6.QtSensors',
        'PySide6.QtSerialPort',
        'PySide6.QtWebSockets',
        'PySide6.QtPdf',
        'PySide6.QtPdfWidgets',
        'PySide6.QtRemoteObjects',
        'PySide6.QtTest',
        'PySide6.QtUiTools',
    ],
    noarchive=False,
)

pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,   # binaries go into COLLECT, not bundled into EXE
    name='STLViewer',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    upx_exclude=[],
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    # icon='icon.ico',
)

coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=True,
    upx_exclude=[],
    name='STLViewer',
)
