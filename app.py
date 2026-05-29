import os
import sys
import time
import hashlib

from PySide6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QSplitter, QTreeView,
    QVBoxLayout, QHBoxLayout, QGridLayout, QGroupBox, QPushButton,
    QLabel, QComboBox, QCheckBox, QColorDialog, QStatusBar,
    QMessageBox, QFrame, QFileSystemModel, QFileDialog, QProgressBar,
    QStackedWidget, QScrollArea, QSlider
)
from PySide6.QtCore import Qt, QDir, QThread, Signal, QEvent, QUrl, QTimer
from PySide6.QtGui import QColor, QIcon, QPainter, QMovie, QPixmap
from PySide6.QtSvgWidgets import QSvgWidget
from PySide6.QtMultimedia import QMediaPlayer, QAudioOutput
from PySide6.QtMultimediaWidgets import QVideoWidget

from collections import deque

from parsers import load_model
from renderer import VTKRendererWidget
from styles import DARK_THEME_QSS

class LoadingOverlay(QWidget):
    """Semi-transparent elegant glassmorphism loading overlay centered on top of its parent."""
    
    def __init__(self, parent):
        super().__init__(parent)
        self.setObjectName("LoadingOverlay")
        self.setStyleSheet("QWidget#LoadingOverlay { background: transparent; }")
        
        # Install event filter to automatically track parent resizing
        if parent:
            parent.installEventFilter(self)
        
        # Overlay Layout
        layout = QVBoxLayout(self)
        layout.setAlignment(Qt.AlignCenter)
        layout.setContentsMargins(0, 0, 0, 0)
        
        # Center glassmorphism loading card
        self.card = QFrame(self)
        self.card.setObjectName("SpinnerCard")
        self.card.setStyleSheet("""
            QFrame#SpinnerCard {
                background-color: #161923;
                border: 1.5px solid #00F0FF;
                border-radius: 12px;
                min-width: 220px;
                max-width: 280px;
                padding: 25px;
            }
            QLabel {
                color: #A4ADC4;
                font-family: 'Segoe UI', Arial, sans-serif;
                background: transparent;
            }
        """)
        card_layout = QVBoxLayout(self.card)
        card_layout.setAlignment(Qt.AlignCenter)
        card_layout.setSpacing(12)
        
        # Glowing Bolt Icon
        self.spinner_label = QLabel("⚡")
        self.spinner_label.setStyleSheet("font-size: 36px; color: #00F0FF; margin-bottom: 5px;")
        self.spinner_label.setAlignment(Qt.AlignCenter)
        
        # Sleek Indeterminate Cyan Line Progress Bar
        self.progress_bar = QProgressBar()
        self.progress_bar.setRange(0, 0)  # Indeterminate sliding state
        self.progress_bar.setTextVisible(False)
        self.progress_bar.setFixedHeight(4)
        self.progress_bar.setFixedWidth(160)
        self.progress_bar.setStyleSheet("""
            QProgressBar {
                background-color: #232836;
                border: none;
                border-radius: 2px;
            }
            QProgressBar::chunk {
                background-color: #00F0FF;
                border-radius: 2px;
            }
        """)
        
        # Text Label
        self.text_label = QLabel("Parsing Geometry...")
        self.text_label.setStyleSheet("font-size: 13px; font-weight: bold; color: #E2E8F0;")
        self.text_label.setAlignment(Qt.AlignCenter)
        
        card_layout.addWidget(self.spinner_label)
        card_layout.addWidget(self.progress_bar)
        card_layout.addWidget(self.text_label)
        
        layout.addWidget(self.card)
        
        self.hide()  # Hidden by default
        
    def paintEvent(self, event):
        """Paint a beautiful semi-transparent glassmorphism overlay background."""
        painter = QPainter(self)
        # 72% opacity dark glass background
        painter.fillRect(self.rect(), QColor(10, 12, 16, 184))
        
    def eventFilter(self, obj, event):
        """Ensure the overlay is perfectly resized and centered on the parent."""
        if obj == self.parent() and event.type() == QEvent.Resize:
            self.setGeometry(0, 0, self.parent().width(), self.parent().height())
        return super().eventFilter(obj, event)
        
    def show_loading(self, message="Parsing Geometry..."):
        self.text_label.setText(message)
        if self.parent():
            self.setGeometry(0, 0, self.parent().width(), self.parent().height())
        self.show()
        self.raise_()


class ModelLoaderWorker(QThread):
    """Asynchronous worker to load 3D meshes without freezing the UI."""
    finished = Signal(object)  # Emits ModelData on success, Exception on failure
    
    def __init__(self, filepath):
        super().__init__()
        self.filepath = filepath
        self.start_time = 0
        
    def run(self):
        self.start_time = time.time()
        try:
            model_data = load_model(self.filepath)
            elapsed = (time.time() - self.start_time) * 1000.0
            model_data.load_time_ms = elapsed
            self.finished.emit(model_data)
        except Exception as e:
            self.finished.emit(e)


class ThumbnailGeneratorWorker(QThread):
    """Asynchronous worker to load 3D model geometry without touching VTK/OpenGL."""
    model_loaded = Signal(str, object)  # Emits (filepath, model_data)
    
    def __init__(self, filepaths):
        super().__init__()
        self.filepaths = filepaths
        self.is_running = True
        
    def run(self):
        from concurrent.futures import ThreadPoolExecutor, as_completed
        max_w = min(os.cpu_count() or 4, 8, len(self.filepaths))
        pool = ThreadPoolExecutor(max_workers=max_w)
        try:
            futures = {pool.submit(load_model, fp): fp for fp in self.filepaths}
            for future in as_completed(futures):
                if not self.is_running:
                    break
                fp = futures[future]
                try:
                    model_data = future.result()
                    if self.is_running:
                        self.model_loaded.emit(fp, model_data)
                except Exception as e:
                    print(f"Error loading geometry for thumbnail {fp}: {e}")
        finally:
            # cancel_futures=True drops pending tasks; wait=False lets running tasks
            # finish in their own threads without blocking the QThread (and therefore
            # without blocking the main thread's generator_worker.wait() call).
            pool.shutdown(wait=False, cancel_futures=True)
                
    def stop(self):
        self.is_running = False


import sqlite3
import time as _time


class ThumbnailCache:
    """SQLite-backed thumbnail cache. Keys on MD5(abs_path); validates against mtime+size."""

    def __init__(self, db_path):
        self._conn = sqlite3.connect(db_path, check_same_thread=False)
        self._conn.execute("""
            CREATE TABLE IF NOT EXISTS thumbnails (
                path_hash  TEXT PRIMARY KEY,
                filepath   TEXT NOT NULL,
                mtime      REAL NOT NULL,
                filesize   INTEGER NOT NULL,
                image_data BLOB NOT NULL,
                created_at REAL NOT NULL
            )
        """)
        self._conn.commit()

    def _hash(self, filepath):
        return hashlib.md5(os.path.abspath(filepath).encode('utf-8')).hexdigest()

    def is_valid(self, filepath):
        """Return True if a fresh thumbnail exists for filepath."""
        try:
            st = os.stat(filepath)
            row = self._conn.execute(
                "SELECT mtime, filesize FROM thumbnails WHERE path_hash=?",
                (self._hash(filepath),)
            ).fetchone()
            return row is not None and row[0] == st.st_mtime and row[1] == st.st_size
        except Exception:
            return False

    def get_pixmap(self, filepath):
        """Return QPixmap for filepath if cached and fresh, else None."""
        try:
            st = os.stat(filepath)
            row = self._conn.execute(
                "SELECT mtime, filesize, image_data FROM thumbnails WHERE path_hash=?",
                (self._hash(filepath),)
            ).fetchone()
            if row is None or row[0] != st.st_mtime or row[1] != st.st_size:
                return None
            from PySide6.QtGui import QPixmap
            from PySide6.QtCore import QByteArray
            pm = QPixmap()
            pm.loadFromData(QByteArray(row[2]))
            return pm if not pm.isNull() else None
        except Exception:
            return None

    def put(self, filepath, png_bytes):
        """Store png_bytes (bytes) for filepath."""
        try:
            st = os.stat(filepath)
            self._conn.execute("""
                INSERT OR REPLACE INTO thumbnails
                    (path_hash, filepath, mtime, filesize, image_data, created_at)
                VALUES (?, ?, ?, ?, ?, ?)
            """, (self._hash(filepath), os.path.abspath(filepath),
                  st.st_mtime, st.st_size, png_bytes, _time.time()))
            self._conn.commit()
        except Exception as e:
            print(f"ThumbnailCache.put error: {e}")

    def close(self):
        self._conn.close()


_thumb_cache: "ThumbnailCache | None" = None

def get_thumb_cache() -> "ThumbnailCache":
    global _thumb_cache
    if _thumb_cache is None:
        db_dir = os.path.join(os.path.expanduser("~"), ".gemini", "antigravity-ide")
        os.makedirs(db_dir, exist_ok=True)
        _thumb_cache = ThumbnailCache(os.path.join(db_dir, "thumbnails.db"))
    return _thumb_cache


# Shared off-screen VTK renderer context to avoid OpenGL context exhaustion under Windows
_shared_offscreen_renderer = None
_shared_offscreen_render_window = None

def generate_vtk_thumbnail(model_data):
    """Headless off-screen VTK rendering of a model; returns PNG bytes or None.
    Must be executed in the main GUI thread to avoid OpenGL context issues.
    Reuses a shared rendering context to avoid wglMakeCurrent handle exhaustion.
    """
    global _shared_offscreen_renderer, _shared_offscreen_render_window
    import math
    import numpy as np
    from vtkmodules.vtkCommonCore import vtkPoints
    from vtkmodules.vtkCommonDataModel import vtkPolyData, vtkCellArray
    from vtkmodules.vtkFiltersCore import vtkPolyDataNormals
    from vtkmodules.vtkRenderingCore import (
        vtkRenderer, vtkRenderWindow, vtkActor, vtkPolyDataMapper,
        vtkLight, vtkWindowToImageFilter,
    )
    from vtkmodules.vtkRenderingOpenGL2 import vtkOpenGLRenderer  # noqa: F401
    from vtkmodules.vtkIOImage import vtkPNGWriter
    from vtkmodules.util import numpy_support

    if len(model_data.vertices) == 0:
        return

    # Create or reuse off-screen rendering pipeline
    if _shared_offscreen_renderer is None:
        _shared_offscreen_renderer = vtkRenderer()
        _shared_offscreen_render_window = vtkRenderWindow()
        _shared_offscreen_render_window.SetOffScreenRendering(1)
        _shared_offscreen_render_window.AddRenderer(_shared_offscreen_renderer)
        _shared_offscreen_render_window.SetSize(228, 200)

    renderer = _shared_offscreen_renderer
    render_window = _shared_offscreen_render_window

    # Clear previous actors/props and lights to start clean
    renderer.RemoveAllViewProps()
    renderer.RemoveAllLights()

    # 1. Slate dark gradient background matching the main viewport
    renderer.SetBackground(0.07, 0.08, 0.11)
    renderer.SetBackground2(0.15, 0.17, 0.22)
    renderer.GradientBackgroundOn()

    # 2. Lighting rig matching the main viewport
    cam_light = vtkLight()
    cam_light.SetLightTypeToCameraLight()
    cam_light.SetDiffuseColor(1.0, 1.0, 1.0)
    cam_light.SetSpecularColor(1.0, 1.0, 1.0)
    cam_light.SetIntensity(0.85)
    renderer.AddLight(cam_light)

    top_light = vtkLight()
    top_light.SetPosition(-1.0, 1.0, 1.0)
    top_light.SetLightTypeToSceneLight()
    top_light.SetDiffuseColor(0.2, 0.25, 0.35)
    top_light.SetIntensity(0.3)
    renderer.AddLight(top_light)

    # 3. Model actor creation
    points = vtkPoints()
    vtk_points_data = numpy_support.numpy_to_vtk(model_data.vertices, deep=True)
    points.SetData(vtk_points_data)

    poly_data = vtkPolyData()
    poly_data.SetPoints(points)

    has_lines = hasattr(model_data, 'lines') and len(model_data.lines) > 0
    
    actor = vtkActor()

    if has_lines:
        num_lines = len(model_data.lines)
        cells_flat = np.empty((num_lines, 3), dtype=np.int64)
        cells_flat[:, 0] = 2
        cells_flat[:, 1:] = model_data.lines
        cells_flat = cells_flat.ravel()

        cells = vtkCellArray()
        vtk_cells_data = numpy_support.numpy_to_vtkIdTypeArray(cells_flat, deep=True)
        cells.SetCells(num_lines, vtk_cells_data)

        poly_data.SetLines(cells)

        mapper = vtkPolyDataMapper()
        mapper.SetInputData(poly_data)
        actor.SetMapper(mapper)

        actor.GetProperty().SetColor(0.0, 0.94, 1.0)
        actor.GetProperty().LightingOff()
        actor.GetProperty().SetLineWidth(1.5)
    else:
        num_triangles = len(model_data.triangles)
        cells_flat = np.empty((num_triangles, 4), dtype=np.int64)
        cells_flat[:, 0] = 3
        cells_flat[:, 1:] = model_data.triangles
        cells_flat = cells_flat.ravel()

        cells = vtkCellArray()
        vtk_cells_data = numpy_support.numpy_to_vtkIdTypeArray(cells_flat, deep=True)
        cells.SetCells(num_triangles, vtk_cells_data)

        poly_data.SetPolys(cells)

        normals_filter = vtkPolyDataNormals()
        normals_filter.SetInputData(poly_data)
        normals_filter.ComputePointNormalsOn()
        normals_filter.ComputeCellNormalsOff()
        normals_filter.ConsistencyOn()
        normals_filter.Update()

        mapper = vtkPolyDataMapper()
        mapper.SetInputConnection(normals_filter.GetOutputPort())
        actor.SetMapper(mapper)

        actor.GetProperty().SetColor(0.0, 0.94, 1.0)
        actor.GetProperty().LightingOn()
        actor.GetProperty().SetInterpolationToPhong()
        actor.GetProperty().SetAmbient(0.18)
        actor.GetProperty().SetDiffuse(0.82)
        actor.GetProperty().SetSpecular(0.45)
        actor.GetProperty().SetSpecularPower(35.0)

    renderer.AddActor(actor)

    # 4. Position camera in isometric view
    renderer.ResetCamera()
    camera = renderer.GetActiveCamera()
    bounds = actor.GetBounds()
    cx = (bounds[0] + bounds[1]) / 2.0
    cy = (bounds[2] + bounds[3]) / 2.0
    cz = (bounds[4] + bounds[5]) / 2.0

    dx = bounds[1] - bounds[0]
    dy = bounds[3] - bounds[2]
    dz = bounds[5] - bounds[4]
    max_dim = max(dx, dy, dz)
    distance = max_dim * 1.85 if max_dim > 0 else 100.0

    camera.SetFocalPoint(cx, cy, cz)
    camera.SetPosition(cx + distance * 0.707, cy - distance * 0.707, cz + distance * 0.707)
    camera.SetViewUp(0, 0, 1)
    renderer.ResetCameraClippingRange()

    # 5. Render offscreen
    render_window.Render()

    # 6. Capture image and write to PNG
    window_to_image_filter = vtkWindowToImageFilter()
    window_to_image_filter.SetInput(render_window)
    window_to_image_filter.SetInputBufferTypeToRGBA()
    window_to_image_filter.Update()

    writer = vtkPNGWriter()
    writer.WriteToMemoryOn()
    writer.SetInputConnection(window_to_image_filter.GetOutputPort())
    writer.Write()

    # 7. Clean up
    renderer.RemoveActor(actor)

    from vtkmodules.util.numpy_support import vtk_to_numpy
    return bytes(vtk_to_numpy(writer.GetResult()))


class ThumbnailCard(QFrame):
    clicked = Signal(str)
    
    def __init__(self, filepath, parent=None):
        super().__init__(parent)
        self.filepath = filepath
        self.setObjectName("ThumbnailCard")
        self.setFixedSize(130, 150)
        self.setCursor(Qt.PointingHandCursor)
        
        # Premium dark glassmorphism card styling
        self.setStyleSheet("""
            QFrame#ThumbnailCard {
                background-color: #1A1D26;
                border: 1.5px solid #232836;
                border-radius: 8px;
            }
            QFrame#ThumbnailCard:hover {
                background-color: #232836;
                border-color: #00F0FF;
            }
        """)
        
        # Layout
        layout = QVBoxLayout(self)
        layout.setContentsMargins(8, 8, 8, 8)
        layout.setSpacing(6)
        
        # Upper Thumbnail Preview Area
        self.thumb_container = QFrame()
        self.thumb_container.setStyleSheet("background-color: #0D0F14; border-radius: 6px; border: none;")
        self.thumb_container.setFixedSize(114, 100)
        
        thumb_layout = QVBoxLayout(self.thumb_container)
        thumb_layout.setContentsMargins(2, 2, 2, 2)
        thumb_layout.setAlignment(Qt.AlignCenter)
        
        self.thumb_layout = thumb_layout  # Store reference for potential async update
        
        ext = os.path.splitext(filepath)[1].lower()
        
        # Add visual based on type
        if ext in ['.jpg', '.jpeg', '.png', '.bmp', '.tiff', '.tif']:
            from PySide6.QtGui import QImageReader
            from PySide6.QtCore import QSize as _QSize
            reader = QImageReader(filepath)
            reader.setScaledSize(_QSize(110, 96))
            img = reader.read()
            if not img.isNull():
                img_lbl = QLabel()
                img_lbl.setAlignment(Qt.AlignCenter)
                img_lbl.setPixmap(QPixmap.fromImage(img))
                img_lbl.setStyleSheet("background: transparent;")
                thumb_layout.addWidget(img_lbl)
            else:
                self._add_icon_label(thumb_layout, "🖼️", "#7A859E")
        elif ext == '.gif':
            from PySide6.QtGui import QImageReader
            from PySide6.QtCore import QSize as _QSize
            reader = QImageReader(filepath)
            reader.setScaledSize(_QSize(110, 96))
            img = reader.read()
            if not img.isNull():
                img_lbl = QLabel()
                img_lbl.setAlignment(Qt.AlignCenter)
                img_lbl.setPixmap(QPixmap.fromImage(img))
                img_lbl.setStyleSheet("background: transparent;")
                thumb_layout.addWidget(img_lbl)
            else:
                self._add_icon_label(thumb_layout, "🎬", "#A855F7")
        elif ext == '.svg':
            svg_widget = QSvgWidget(filepath)
            svg_widget.setFixedSize(90, 80)
            svg_widget.setStyleSheet("background: transparent;")
            thumb_layout.addWidget(svg_widget)
        elif ext == '.webm':
            self._add_icon_label(thumb_layout, "🎥", "#F43F5E")
        elif ext in ['.stl', '.3mf', '.obj', '.gcode', '.gco']:
            pixmap = get_thumb_cache().get_pixmap(filepath)
            if pixmap is not None and not pixmap.isNull():
                img_lbl = QLabel()
                img_lbl.setAlignment(Qt.AlignCenter)
                img_lbl.setPixmap(pixmap.scaled(110, 96, Qt.KeepAspectRatio, Qt.SmoothTransformation))
                img_lbl.setStyleSheet("background: transparent;")
                thumb_layout.addWidget(img_lbl)
            else:
                self._add_placeholder(ext, thumb_layout)
        else:
            self._add_icon_label(thumb_layout, "📄", "#7A859E")
            
        layout.addWidget(self.thumb_container)
        
        # Lower text label
        self.name_label = QLabel()
        filename = os.path.basename(filepath)
        self.name_label.setText(filename)
        self.name_label.setAlignment(Qt.AlignCenter)
        self.name_label.setStyleSheet("color: #E2E8F0; font-size: 10px; font-weight: bold; border: none; background: transparent;")
        
        # Elide text if too long
        font_metrics = self.name_label.fontMetrics()
        elided = font_metrics.elidedText(filename, Qt.ElideMiddle, 110)
        self.name_label.setText(elided)
        
        # Tooltip with full name
        self.setToolTip(filename)
        
        layout.addWidget(self.name_label)
        
    def _add_icon_label(self, layout, icon_char, color_hex, text=""):
        container = QWidget()
        container.setStyleSheet("background: transparent;")
        layout_c = QVBoxLayout(container)
        layout_c.setContentsMargins(0, 0, 0, 0)
        layout_c.setSpacing(2)
        layout_c.setAlignment(Qt.AlignCenter)
        
        icon_lbl = QLabel(icon_char)
        icon_lbl.setStyleSheet(f"font-size: 28px; color: {color_hex}; background: transparent;")
        icon_lbl.setAlignment(Qt.AlignCenter)
        layout_c.addWidget(icon_lbl)
        
        if text:
            txt_lbl = QLabel(text)
            txt_lbl.setStyleSheet(f"font-size: 9px; font-weight: bold; color: {color_hex}; background: transparent; letter-spacing: 0.5px;")
            txt_lbl.setAlignment(Qt.AlignCenter)
            layout_c.addWidget(txt_lbl)
            
        layout.addWidget(container)
        
    def _add_placeholder(self, ext, layout):
        if ext == '.stl':
            self._add_icon_label(layout, "📐", "#06B6D4", "STL")
        elif ext == '.3mf':
            self._add_icon_label(layout, "🧱", "#EAB308", "3MF")
        elif ext == '.obj':
            self._add_icon_label(layout, "🏺", "#F97316", "OBJ")
        elif ext in ['.gcode', '.gco']:
            self._add_icon_label(layout, "🧵", "#22C55E", "G-CODE")

    def update_thumbnail(self, pixmap):
        """Asynchronously swap dynamic offscreen 3D render when generator finishes."""
        while self.thumb_layout.count() > 0:
            item = self.thumb_layout.takeAt(0)
            w = item.widget()
            if w:
                w.deleteLater()

        if pixmap is not None and not pixmap.isNull():
            img_lbl = QLabel()
            img_lbl.setAlignment(Qt.AlignCenter)
            img_lbl.setPixmap(pixmap.scaled(110, 96, Qt.KeepAspectRatio, Qt.SmoothTransformation))
            img_lbl.setStyleSheet("background: transparent;")
            self.thumb_layout.addWidget(img_lbl)

    def mousePressEvent(self, event):
        if event.button() == Qt.LeftButton:
            self.clicked.emit(self.filepath)
        super().mousePressEvent(event)


class ThumbnailGridWidget(QWidget):
    card_clicked = Signal(str)
    status_message = Signal(str)   # forwarded to MainWindow status bar

    _THUMB_SIZE_LIMIT = 10 * 1024 * 1024  # skip VTK render for files > 10 MB
    _CARD_BATCH = 50                       # cards created per event-loop tick

    def __init__(self, parent=None):
        super().__init__(parent)
        self.cards = {}           # filepath -> ThumbnailCard (O(1) lookup)
        self._ordered_paths = []  # insertion order for grid layout
        self.generator_worker = None
        self._render_queue = deque()
        self._render_timer = QTimer(self)
        self._render_timer.setSingleShot(True)
        self._render_timer.timeout.connect(self._render_next)
        self._total_pending = 0
        self._rendered_count = 0
        self._total_file_count = 0

        # Deferred card-creation state
        self._pending_card_paths = deque()
        self._pending_uncached = []
        self._card_timer = QTimer(self)
        self._card_timer.setSingleShot(True)
        self._card_timer.timeout.connect(self._create_card_batch)

        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)

        self._progress_bar = QProgressBar()
        self._progress_bar.setTextVisible(True)
        self._progress_bar.setFixedHeight(18)
        self._progress_bar.setFormat("Generating thumbnails: 0 / 0")
        self._progress_bar.setStyleSheet(
            "QProgressBar { background: #11131A; border: none; border-bottom: 1px solid #1E222D;"
            " color: #7A859E; font-size: 10px; text-align: center; }"
            "QProgressBar::chunk { background: #00F0FF; }"
        )
        self._progress_bar.hide()
        outer.addWidget(self._progress_bar)

        grid_container = QWidget()
        self._grid_layout = QGridLayout(grid_container)
        self._grid_layout.setSpacing(15)
        self._grid_layout.setContentsMargins(15, 15, 15, 15)
        self._grid_layout.setAlignment(Qt.AlignLeft | Qt.AlignTop)
        outer.addWidget(grid_container, stretch=1)

        self.layout = self._grid_layout  # backward-compat alias

    def _stop_workers(self):
        self._card_timer.stop()
        self._pending_card_paths.clear()
        self._pending_uncached.clear()
        self._render_timer.stop()
        self._render_queue.clear()
        if self.generator_worker and self.generator_worker.isRunning():
            self.generator_worker.stop()
            self.generator_worker.wait()
            self.generator_worker = None
        self._progress_bar.hide()

    def set_files(self, filepaths):
        self._stop_workers()

        for card in self.cards.values():
            self._grid_layout.removeWidget(card)
            card.deleteLater()
        self.cards = {}
        self._ordered_paths = []
        self._rendered_count = 0
        self._total_pending = 0

        mesh_exts = {'.stl', '.3mf', '.obj', '.gcode', '.gco'}
        cache = get_thumb_cache()
        uncached_3d = []
        for filepath in filepaths:
            ext = os.path.splitext(filepath)[1].lower()
            if ext in mesh_exts and not cache.is_valid(filepath):
                try:
                    if os.path.getsize(filepath) <= self._THUMB_SIZE_LIMIT:
                        uncached_3d.append(filepath)
                except OSError:
                    pass

        # Kick off deferred card creation (50 cards per event-loop tick).
        # _ordered_paths is populated inside _create_card_batch so rearrange_grid
        # only sees paths that already have a card widget.
        self._total_file_count = len(filepaths)
        self._pending_card_paths = deque(filepaths)
        self._pending_uncached = uncached_3d
        self._card_timer.start(0)

    def _create_card_batch(self):
        """Create up to _CARD_BATCH cards, then yield to the event loop."""
        for _ in range(self._CARD_BATCH):
            if not self._pending_card_paths:
                break
            filepath = self._pending_card_paths.popleft()
            card = ThumbnailCard(filepath)
            card.clicked.connect(self.card_clicked.emit)
            self.cards[filepath] = card
            self._ordered_paths.append(filepath)  # only add once the card exists

        self.rearrange_grid()

        n = len(self._ordered_paths)
        total = self._total_file_count
        if self._pending_card_paths:
            self.status_message.emit(f"Loading folder: {n} / {total} files ...")
            self._card_timer.start(0)
        else:
            self.status_message.emit(f"Folder loaded: {total} files")
            self._start_geometry_loading(self._pending_uncached)
            self._pending_uncached = []

    def _start_geometry_loading(self, uncached_3d):
        if not uncached_3d:
            return
        self._total_pending = len(uncached_3d)
        self._rendered_count = 0
        self._progress_bar.setRange(0, self._total_pending)
        self._progress_bar.setValue(0)
        self._progress_bar.setFormat(
            f"Generating thumbnails: 0 / {self._total_pending}"
        )
        self._progress_bar.show()
        self.status_message.emit(
            f"Generating {self._total_pending} thumbnails (files > 10 MB skipped) ..."
        )
        self.generator_worker = ThumbnailGeneratorWorker(uncached_3d)
        self.generator_worker.model_loaded.connect(self._on_geom_loaded)
        self.generator_worker.start()

    def _on_geom_loaded(self, filepath, model_data):
        """Geometry loaded on background thread — queue it for main-thread VTK render."""
        self._render_queue.append((filepath, model_data))
        if not self._render_timer.isActive():
            self._render_timer.start(0)  # fire on next idle event-loop tick

    def _render_next(self):
        """Render one queued thumbnail per event-loop tick to keep UI responsive."""
        if not self._render_queue:
            return
        filepath, model_data = self._render_queue.popleft()
        basename = os.path.basename(filepath)
        self.status_message.emit(
            f"Rendering thumbnail {self._rendered_count + 1} / {self._total_pending}  —  {basename}"
        )
        try:
            png_bytes = generate_vtk_thumbnail(model_data)
            if png_bytes:
                cache = get_thumb_cache()
                cache.put(filepath, png_bytes)
                pixmap = cache.get_pixmap(filepath)
                if pixmap:
                    self._on_thumbnail_ready(filepath, pixmap)
        except Exception as e:
            print(f"Error rendering thumbnail for {filepath}: {e}")
        self._rendered_count += 1
        self._progress_bar.setValue(self._rendered_count)
        self._progress_bar.setFormat(
            f"Generating thumbnails: {self._rendered_count} / {self._total_pending}"
        )
        if self._rendered_count >= self._total_pending:
            self._progress_bar.hide()
            self.status_message.emit(
                f"All {self._total_pending} thumbnails ready  —  {self._total_file_count} files in folder"
            )
        if self._render_queue:
            self._render_timer.start(0)

    def _on_thumbnail_ready(self, filepath, pixmap):
        card = self.cards.get(filepath)
        if card:
            card.update_thumbnail(pixmap)

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self.rearrange_grid()

    def rearrange_grid(self):
        if not self._ordered_paths:
            return
        width = self.width()
        card_outer_width = 145  # card width (130) + grid spacing (15)
        cols = max(1, width // card_outer_width)
        for filepath in self._ordered_paths:
            self._grid_layout.removeWidget(self.cards[filepath])
        for idx, filepath in enumerate(self._ordered_paths):
            r = idx // cols
            c = idx % cols
            self._grid_layout.addWidget(self.cards[filepath], r, c)


class ClickableLabel(QLabel):
    """QLabel that copies its toolTip (full path) to clipboard on left-click."""
    copied = Signal(str)

    def __init__(self, text="", parent=None):
        super().__init__(text, parent)
        self.setCursor(Qt.PointingHandCursor)
        self.setStyleSheet(
            "color: #00C9D4; font-size: 10px; text-decoration: underline; background: transparent;"
        )

    def mousePressEvent(self, event):
        if event.button() == Qt.LeftButton:
            full_path = self.toolTip()
            if full_path:
                QApplication.clipboard().setText(full_path)
                self.copied.emit(full_path)
        super().mousePressEvent(event)


class MainWindow(QMainWindow):
    """Main window for the STL/3MF file viewer application."""
    
    def __init__(self):
        super().__init__()
        self.setWindowTitle("STL/3MF Viewer (3MF, OBJ, STL, G-Code, images)")
        self.resize(1280, 800)
        
        # Apply premium dark stylesheet
        self.setStyleSheet(DARK_THEME_QSS)
        
        self.active_worker = None
        self.precache_worker = None
        self._current_filepath = None
        self._precache_render_queue = deque()
        self._precache_render_timer = QTimer(self)
        self._precache_render_timer.setSingleShot(True)
        self._precache_render_timer.timeout.connect(self._precache_render_next)
        self.current_model = None
        self.current_pixmap = None   # original unscaled pixmap for static images
        self.image_zoom = 1.0        # zoom factor relative to original pixel size
        self.current_media_type = None  # 'image' | 'gif' | 'svg'
        self._img_drag_active = False
        self._img_drag_start_pos = None   # global cursor pos at press
        self._img_drag_scroll_start = None  # (h_value, v_value) at press
        
        self._init_ui()
        self.thumbnail_grid.status_message.connect(self.status_bar.showMessage)
        self.meta_labels["file_path"].copied.connect(
            lambda p: self.status_bar.showMessage(f"Path copied to clipboard: {p}")
        )
        self._setup_file_browser()
        
    def _init_ui(self):
        """Assemble layout and widgets."""
        # Main central splitter
        central_splitter = QSplitter(Qt.Horizontal)
        self.setCentralWidget(central_splitter)
        
        # --- LEFT SIDEBAR (File System + Metadata) ---
        sidebar = QFrame()
        sidebar.setObjectName("SidebarFrame")
        sidebar_layout = QVBoxLayout(sidebar)
        sidebar_layout.setContentsMargins(15, 20, 15, 15)
        sidebar_layout.setSpacing(15)
        
        # Sidebar Logo / Header
        header_layout = QHBoxLayout()
        logo_label = QLabel("⚡")
        logo_label.setStyleSheet("font-size: 20px; color: #00F0FF;")
        title_label = QLabel("STL 3D View")
        title_label.setObjectName("HeaderLabel")
        header_layout.addWidget(logo_label)
        header_layout.addWidget(title_label)
        header_layout.addStretch()
        sidebar_layout.addLayout(header_layout)
        
        # File Browser Tree
        browser_group = QGroupBox("FileSystem Browser")
        browser_layout = QVBoxLayout(browser_group)
        
        # Change directory controls
        cd_layout = QHBoxLayout()
        from PySide6.QtWidgets import QLineEdit
        self.dir_label = QLineEdit("Loading...")
        self.dir_label.setStyleSheet(
            "QLineEdit { color: #7A859E; font-size: 11px; font-weight: bold; "
            "background: #11131A; border: 1px solid #1E222D; border-radius: 4px; padding: 3px 6px; }"
            "QLineEdit:focus { border-color: #00F0FF; color: #E2E8F0; }"
        )
        self.dir_label.setPlaceholderText("Enter folder path...")
        self.dir_label.returnPressed.connect(self._navigate_to_typed_path)
        self.dir_label.editingFinished.connect(self._on_path_editing_finished)
        self._path_navigated_via_return = False

        up_btn = QPushButton("↑")
        up_btn.setFixedWidth(28)
        up_btn.setToolTip("Go up one folder level")
        up_btn.clicked.connect(self._navigate_up)
        up_btn.setStyleSheet("padding: 4px 4px; font-size: 14px;")

        change_dir_btn = QPushButton("Browse...")
        change_dir_btn.clicked.connect(self._change_root_directory)
        change_dir_btn.setStyleSheet("padding: 4px 8px; font-size: 11px;")

        cd_layout.addWidget(self.dir_label, stretch=1)
        cd_layout.addWidget(up_btn)
        cd_layout.addWidget(change_dir_btn)
        browser_layout.addLayout(cd_layout)
        
        # Media visibility toggle checkbox
        toggle_layout = QHBoxLayout()
        self.show_images_cb = QCheckBox("Show Images & Media")
        self.show_images_cb.setChecked(False)
        self.show_images_cb.stateChanged.connect(self._toggle_image_visibility)
        toggle_layout.addWidget(self.show_images_cb)
        browser_layout.addLayout(toggle_layout)
        
        self.tree_view = QTreeView()
        self.tree_view.setHeaderHidden(True)
        self.tree_view.setHorizontalScrollBarPolicy(Qt.ScrollBarAsNeeded)
        self.tree_view.setVerticalScrollBarPolicy(Qt.ScrollBarAsNeeded)
        self.tree_view.setHorizontalScrollMode(QTreeView.ScrollPerPixel)
        browser_layout.addWidget(self.tree_view)
        sidebar_layout.addWidget(browser_group)
        
        # Metadata / Mesh Properties Card
        self.meta_group = QGroupBox("Mesh Details")
        meta_layout = QGridLayout(self.meta_group)
        meta_layout.setSpacing(10)
        
        self.meta_labels = {}
        fields = [
            ("file_name", "Filename:"),
            ("file_size", "File Size:"),
            ("file_path", "Path:"),
            ("format", "Format:"),
            ("vertices", "Vertices:"),
            ("triangles", "Triangles:"),
            ("bounds", "Dimensions:"),
            ("volume", "Est. Volume:")
        ]

        self.meta_row_labels = {}
        for idx, (key, label_text) in enumerate(fields):
            label = QLabel(label_text)
            self.meta_row_labels[key] = label
            if key == "file_path":
                val_label = ClickableLabel("-")
                val_label.setObjectName("ValueLabel")
                val_label.setWordWrap(True)
                val_label.setToolTip("")
            else:
                val_label = QLabel("-")
                val_label.setObjectName("ValueLabel")
                val_label.setWordWrap(True)

            meta_layout.addWidget(label, idx, 0)
            meta_layout.addWidget(val_label, idx, 1)
            self.meta_labels[key] = val_label
            
        sidebar_layout.addWidget(self.meta_group)
        central_splitter.addWidget(sidebar)
        
        # --- RIGHT PANEL (3D Viewer, Media Viewer & Controls) ---
        right_panel = QFrame()
        right_layout = QVBoxLayout(right_panel)
        right_layout.setContentsMargins(0, 0, 0, 0)
        right_layout.setSpacing(0)
        
        # Stacked Widget to hold different viewport pages
        self.viewport_stack = QStackedWidget()
        right_layout.addWidget(self.viewport_stack, stretch=1)
        
        # Page 0: 3D VTK Viewport
        self.vtk_widget = VTKRendererWidget()
        self.viewport_stack.addWidget(self.vtk_widget)
        
        # Centered glassmorphism loading overlay on top of VTK canvas
        self.loading_overlay = LoadingOverlay(self.vtk_widget)
        
        # Page 1: Image / GIF / SVG Viewer
        self.image_viewer_area = QScrollArea()
        self.image_viewer_area.setWidgetResizable(False)
        self.image_viewer_area.setAlignment(Qt.AlignCenter)
        self.image_viewer_area.setStyleSheet("background-color: #0A0C10; border: none;")
        self.image_viewer_area.viewport().installEventFilter(self)
        
        self.image_container = QWidget()
        image_container_layout = QVBoxLayout(self.image_container)
        image_container_layout.setAlignment(Qt.AlignCenter)
        image_container_layout.setContentsMargins(0, 0, 0, 0)
        
        # Label to display static QPixmaps and QMovies (GIFs)
        self.image_label = QLabel()
        self.image_label.setAlignment(Qt.AlignCenter)
        self.image_label.setStyleSheet("background: transparent;")
        image_container_layout.addWidget(self.image_label)
        
        # QSvgWidget to display SVGs sharply
        self.svg_widget = QSvgWidget()
        self.svg_widget.setStyleSheet("background: transparent;")
        image_container_layout.addWidget(self.svg_widget)
        self.svg_widget.hide() # Hidden by default
        
        self.image_viewer_area.setWidget(self.image_container)
        self.viewport_stack.addWidget(self.image_viewer_area)
        
        # Page 2: Video Player for WebM
        self.video_container = QFrame()
        self.video_container.setStyleSheet("background-color: #0A0C10; border: none;")
        video_container_layout = QVBoxLayout(self.video_container)
        video_container_layout.setContentsMargins(0, 0, 0, 0)
        video_container_layout.setSpacing(0)
        
        # Video Output Widget
        self.video_widget = QVideoWidget()
        self.video_widget.setStyleSheet("background-color: #000000;")
        video_container_layout.addWidget(self.video_widget, stretch=1)
        
        # Video Control Panel
        self.video_controls = QFrame()
        self.video_controls.setObjectName("RightControlDeck")
        self.video_controls.setFixedHeight(50)
        self.video_controls.setStyleSheet("background-color: #151821; border-top: 1px solid #232836;")
        controls_layout = QHBoxLayout(self.video_controls)
        controls_layout.setContentsMargins(15, 5, 15, 5)
        controls_layout.setSpacing(15)
        
        # Play/Pause button
        self.play_btn = QPushButton("Play")
        self.play_btn.setFixedWidth(80)
        self.play_btn.clicked.connect(self._toggle_video_playback)
        controls_layout.addWidget(self.play_btn)
        
        # Timeline Seeker Slider
        self.seeker = QSlider(Qt.Horizontal)
        self.seeker.setRange(0, 0)
        self.seeker.sliderMoved.connect(self._set_video_position)
        controls_layout.addWidget(self.seeker)
        
        # Time counter label
        self.time_label = QLabel("00:00 / 00:00")
        self.time_label.setStyleSheet("color: #7A859E; font-size: 11px;")
        controls_layout.addWidget(self.time_label)
        
        # Mute button
        self.mute_btn = QPushButton("Mute")
        self.mute_btn.setFixedWidth(70)
        self.mute_btn.clicked.connect(self._toggle_video_mute)
        controls_layout.addWidget(self.mute_btn)
        
        video_container_layout.addWidget(self.video_controls)
        self.viewport_stack.addWidget(self.video_container)
        
        # Page 3: Folder Thumbnail Grid
        self.thumbnail_area = QScrollArea()
        self.thumbnail_area.setWidgetResizable(True)
        self.thumbnail_area.setStyleSheet("background-color: #0A0C10; border: none;")
        self.thumbnail_grid = ThumbnailGridWidget()
        self.thumbnail_area.setWidget(self.thumbnail_grid)
        self.viewport_stack.addWidget(self.thumbnail_area)
        self.thumbnail_grid.card_clicked.connect(self._on_thumbnail_clicked)
        
        # Initialize Multimedia Player
        self.media_player = QMediaPlayer()
        self.audio_output = QAudioOutput()
        self.media_player.setAudioOutput(self.audio_output)
        self.media_player.setVideoOutput(self.video_widget)
        
        # Listen to player state events
        self.media_player.positionChanged.connect(self._video_position_changed)
        self.media_player.durationChanged.connect(self._video_duration_changed)
        
        # View & Control Deck at the bottom of the viewport
        control_deck = QFrame()
        control_deck.setObjectName("RightControlDeck")
        control_deck_layout = QHBoxLayout(control_deck)
        control_deck_layout.setContentsMargins(20, 15, 20, 15)
        control_deck_layout.setSpacing(20)
        
        # Group 1: View Presets (Isometric, Top, Front, Side)
        views_layout = QHBoxLayout()
        views_layout.setSpacing(6)
        
        presets = [
            ("ISO", "isometric"),
            ("TOP", "top"),
            ("FRONT", "front"),
            ("RIGHT", "right")
        ]
        for name, preset_key in presets:
            btn = QPushButton(name)
            btn.clicked.connect(lambda checked=False, p=preset_key: self.vtk_widget.set_view_preset(p))
            views_layout.addWidget(btn)
            
        control_deck_layout.addLayout(views_layout)
        
        # Separator Line
        v_line = QFrame()
        v_line.setFrameShape(QFrame.VLine)
        v_line.setStyleSheet("color: #232836;")
        control_deck_layout.addWidget(v_line)
        
        # Group 2: Shading Options
        shading_layout = QHBoxLayout()
        shading_layout.addWidget(QLabel("Shading:"))
        self.shading_combo = QComboBox()
        self.shading_combo.addItem("Smooth Shading", "smooth")
        self.shading_combo.addItem("Flat Shading", "flat")
        self.shading_combo.addItem("Wireframe", "wireframe")
        self.shading_combo.addItem("Point Cloud", "points")
        self.shading_combo.currentIndexChanged.connect(self._shading_changed)
        shading_layout.addWidget(self.shading_combo)
        control_deck_layout.addLayout(shading_layout)
        
        # Separator Line
        v_line2 = QFrame()
        v_line2.setFrameShape(QFrame.VLine)
        v_line2.setStyleSheet("color: #232836;")
        control_deck_layout.addWidget(v_line2)
        
        # Group 3: Color picker and Grid/Auto-Rotate options
        color_btn = QPushButton("Model Color")
        color_btn.setObjectName("AccentButton")
        color_btn.clicked.connect(self._select_color)
        control_deck_layout.addWidget(color_btn)
        

        
        self.rotate_cb = QCheckBox("Auto-Rotate")
        self.rotate_cb.stateChanged.connect(self._rotate_changed)
        control_deck_layout.addWidget(self.rotate_cb)

        v_line3 = QFrame()
        v_line3.setFrameShape(QFrame.VLine)
        v_line3.setStyleSheet("color: #232836;")
        control_deck_layout.addWidget(v_line3)

        self.prev_btn = QPushButton("◀ Prev")
        self.prev_btn.clicked.connect(self._load_prev_file)
        control_deck_layout.addWidget(self.prev_btn)

        self.next_btn = QPushButton("Next ▶")
        self.next_btn.clicked.connect(self._load_next_file)
        control_deck_layout.addWidget(self.next_btn)

        control_deck_layout.addStretch()
        right_layout.addWidget(control_deck)
        
        central_splitter.addWidget(right_panel)
        
        # Configure layout proportions (sidebar is 25% window width)
        central_splitter.setSizes([320, 960])
        
        # Status Bar
        self.status_bar = QStatusBar()
        self.setStatusBar(self.status_bar)
        self.status_bar.showMessage("Ready. Select a file in the browser tree to preview.")
        self.status_bar.setStyleSheet("QStatusBar { background-color: #0A0C10; color: #A4ADC4; border-top: 1px solid #1F2432; }")

    def _setup_file_browser(self):
        """Configure directory tree browser filtering for STL & 3MF files."""
        self.file_model = QFrameSystemModel(self)
        self.file_model.setRootPath(QDir.rootPath())
        
        # Filters
        self.file_model.setNameFilters(["*.stl", "*.3mf", "*.obj", "*.gcode", "*.gco"])
        self.file_model.setNameFilterDisables(False) # Hides unmatching files
        self.file_model.setFilter(QDir.AllDirs | QDir.Files | QDir.NoDotAndDotDot)
        
        self.tree_view.setModel(self.file_model)
        
        # Set root directory to C:\
        current_dir = "C:\\"
        self.tree_view.setRootIndex(self.file_model.index(current_dir))
        self.dir_label.setText(current_dir)
        
        # Adjust column visibility (hide sizes, types, and modification dates for a minimalist clean tree)
        for col in range(1, self.file_model.columnCount()):
            self.tree_view.setColumnHidden(col, True)
            
        self.tree_view.selectionModel().selectionChanged.connect(self._file_selected)
        self.tree_view.expanded.connect(self._on_tree_expanded)
        
        # Pre-cache startup directory thumbnails in background
        self._precache_directory_thumbnails(current_dir)

    def _on_tree_expanded(self, index):
        """When a folder is expanded in the tree, show its thumbnail grid and start scanning."""
        filepath = self.file_model.filePath(index)
        if os.path.isdir(filepath):
            self._open_folder(filepath)

    def _navigate_to_typed_path(self):
        """Navigate the tree to the path the user typed in the path bar."""
        self._path_navigated_via_return = True
        typed = self.dir_label.text().strip()
        path = os.path.normpath(typed)
        if os.path.isdir(path):
            self.tree_view.setRootIndex(self.file_model.index(path))
            self.dir_label.setText(path)
            self.dir_label.clearFocus()
            self.status_bar.showMessage(f"Navigated to: {path}")
            self._precache_directory_thumbnails(path)
        else:
            self.status_bar.showMessage(f"Invalid path: {path}")
            current_root = self.file_model.filePath(self.tree_view.rootIndex())
            self.dir_label.setText(os.path.normpath(current_root))

    def _on_path_editing_finished(self):
        """Trigger navigation only when focus is lost (not after Enter, which already navigated)."""
        if self._path_navigated_via_return:
            self._path_navigated_via_return = False
            return
        self._navigate_to_typed_path()

    def _change_root_directory(self):
        """Open native folder selection dialog and change file tree browser root."""
        current_root = self.file_model.filePath(self.tree_view.rootIndex())
        selected_dir = QFileDialog.getExistingDirectory(
            self, 
            "Select Directory to Browse", 
            current_root
        )
        if selected_dir:
            self.tree_view.setRootIndex(self.file_model.index(selected_dir))
            norm_path = os.path.normpath(selected_dir)
            self.dir_label.setText(norm_path)
            self.status_bar.showMessage(f"Browsing directory: {norm_path}")
            
            # Pre-cache directory thumbnails in background
            self._precache_directory_thumbnails(selected_dir)

    def _navigate_up(self):
        """Navigate the file tree root up one directory level."""
        current_root = os.path.normpath(self.file_model.filePath(self.tree_view.rootIndex()))
        parent = os.path.dirname(current_root)
        if parent and parent != current_root:
            self.tree_view.setRootIndex(self.file_model.index(parent))
            self.dir_label.setText(parent)
            self.status_bar.showMessage(f"Navigated to: {parent}")
            self._precache_directory_thumbnails(parent)

    def _file_selected(self, selected, deselected):
        """Load file (mesh or media) when selected in the browser."""
        indexes = selected.indexes()
        if not indexes:
            return
            
        index = indexes[0]
        filepath = self.file_model.filePath(index)
        
        if os.path.isdir(filepath):
            self._open_folder(filepath)
            return
            
        ext = os.path.splitext(filepath)[1].lower()
        
        # Media and Mesh extensions checklists
        media_exts = ['.jpg', '.jpeg', '.png', '.gif', '.tiff', '.tif', '.svg', '.bmp', '.webm']
        mesh_exts = ['.stl', '.3mf', '.obj', '.gcode', '.gco']
        
        if ext not in mesh_exts and ext not in media_exts:
            return
            
        # 1. Terminate any active 3D thread and background thumbnail generator
        if hasattr(self, 'thumbnail_grid') and self.thumbnail_grid:
            if hasattr(self.thumbnail_grid, '_stop_workers'):
                self.thumbnail_grid._stop_workers()
                    
        if self.active_worker and self.active_worker.isRunning():
            self.active_worker.terminate()
            self.active_worker.wait()
            
        # 2. Reset active media players
        self._cleanup_media_playback()
        
        if ext in media_exts:
            # Hide loading overlay and load image/video immediately
            self.loading_overlay.hide()
            self._load_media_file(filepath, ext)
        else:
            # Switch Stack back to VTK
            self.viewport_stack.setCurrentIndex(0)
            
            basename = os.path.basename(filepath)
            try:
                size_str = self._format_file_size(os.path.getsize(filepath))
            except OSError:
                size_str = ""
            self.status_bar.showMessage(
                f"Reading: {basename}  ({size_str})" if size_str else f"Reading: {basename}"
            )
            self._reset_metadata_labels()

            # Show loading spinner overlay centered on viewport
            self.loading_overlay.show_loading(f"Parsing {basename} ...")
            
            # Initialize async loader
            self._current_filepath = filepath
            self.active_worker = ModelLoaderWorker(filepath)
            self.active_worker.finished.connect(self._on_model_loaded)
            self.active_worker.start()

    def _open_folder(self, filepath):
        """Show the thumbnail grid for a folder and start scanning its contents."""
        if self.active_worker and self.active_worker.isRunning():
            self.active_worker.terminate()
            self.active_worker.wait()
        self._cleanup_media_playback()

        # Stop precache geometry worker and any queued VTK renders so they don't
        # compete with the thumbnail grid's own render timer.
        if hasattr(self, '_precache_render_timer'):
            self._precache_render_timer.stop()
        if hasattr(self, '_precache_render_queue'):
            self._precache_render_queue.clear()
        if hasattr(self, 'precache_worker') and self.precache_worker and self.precache_worker.isRunning():
            self.precache_worker.stop()
            self.precache_worker.wait()

        self.viewport_stack.setCurrentIndex(3)

        supported_files = []
        media_exts = ['.jpg', '.jpeg', '.png', '.gif', '.tiff', '.tif', '.svg', '.bmp', '.webm']
        mesh_exts = ['.stl', '.3mf', '.obj', '.gcode', '.gco']
        try:
            for entry in os.scandir(filepath):
                try:
                    if entry.is_file():
                        ext = os.path.splitext(entry.name)[1].lower()
                        is_show_media = self.show_images_cb.isChecked()
                        if ext in mesh_exts or (is_show_media and ext in media_exts):
                            supported_files.append(entry.path)
                except Exception as fe:
                    print(f"Skipping file scan entry due to error: {fe}")
        except Exception as e:
            self.status_bar.showMessage(f"Error reading directory: {str(e)}")
            return

        supported_files.sort(key=lambda x: os.path.basename(x).lower())
        self.thumbnail_grid.set_files(supported_files)

        self.meta_group.setTitle("Folder Details")
        self.meta_row_labels["triangles"].setText("Total Files:")
        self.meta_row_labels["volume"].setText("Total Size:")
        self.meta_labels["file_name"].setText(os.path.basename(filepath) or filepath)
        norm = os.path.normpath(filepath)
        self.meta_labels["file_size"].setText("N/A")
        fm = self.meta_labels["file_path"].fontMetrics()
        self.meta_labels["file_path"].setText(fm.elidedText(norm, Qt.ElideLeft, 160))
        self.meta_labels["file_path"].setToolTip(norm)
        self.meta_labels["format"].setText("Directory")
        self.meta_labels["vertices"].setText("N/A")
        self.meta_labels["triangles"].setText(f"{len(supported_files)}")
        self.meta_labels["bounds"].setText("N/A")

        total_bytes = 0
        for f in supported_files:
            try:
                total_bytes += os.path.getsize(f)
            except:
                pass
        self.meta_labels["volume"].setText(self._format_file_size(total_bytes))
        self.dir_label.setText(os.path.normpath(filepath))
        folder_name = os.path.basename(filepath) or filepath
        self.status_bar.showMessage(
            f"Folder: {folder_name}  —  {len(supported_files)} files  —  {self._format_file_size(total_bytes)}"
        )

    def _on_thumbnail_clicked(self, filepath):
        """When a thumbnail is clicked, select that file in the tree to open it."""
        # Stop background thumbnail generator to prevent OpenGL context collision
        if hasattr(self, 'thumbnail_grid') and self.thumbnail_grid:
            if hasattr(self.thumbnail_grid, '_stop_workers'):
                self.thumbnail_grid._stop_workers()
                    
        index = self.file_model.index(filepath)
        if index.isValid():
            from PySide6.QtCore import QItemSelectionModel
            self.tree_view.selectionModel().clearSelection()
            self.tree_view.setCurrentIndex(index)
            self.tree_view.selectionModel().select(index, QItemSelectionModel.Select)

    def _on_model_loaded(self, result):
        """Callback when the thread finishes loading the mesh model."""
        self.loading_overlay.hide()
        
        if isinstance(result, Exception):
            # Loader failed
            self.status_bar.showMessage("Error loading model.")
            QMessageBox.critical(
                self, 
                "Loading Error", 
                f"Failed to load the 3D model:\n{str(result)}"
            )
            return
            
        # Bind and render model in VTK Viewport
        self.current_model = result
        self.vtk_widget.set_model(result)
        
        # Reset shading combo to smooth by default on new models
        self.shading_combo.setCurrentIndex(0)
        self.rotate_cb.setChecked(False)
        
        # Update metadata card
        self._update_metadata_ui(result)
        
        is_gcode = hasattr(result, 'lines') and len(result.lines) > 0
        if is_gcode:
            self.status_bar.showMessage(
                f"Loaded: {result.filename}  —  {result.format}  —  "
                f"{result.num_lines:,} lines  —  {result.load_time_ms:.0f} ms"
            )
        else:
            self.status_bar.showMessage(
                f"Loaded: {result.filename}  —  {result.format}  —  "
                f"{result.num_vertices:,} vertices, {result.num_triangles:,} triangles  —  {result.load_time_ms:.0f} ms"
            )

    def _update_metadata_ui(self, model):
        """Bind dynamic text to metadata cards."""
        self.meta_labels["file_name"].setText(model.filename)

        fp = self._current_filepath or ""
        try:
            size_str = self._format_file_size(os.path.getsize(fp)) if fp else "-"
        except OSError:
            size_str = "-"
        self.meta_labels["file_size"].setText(size_str)

        norm = os.path.normpath(fp) if fp else "-"
        fm = self.meta_labels["file_path"].fontMetrics()
        elided = fm.elidedText(norm, Qt.ElideLeft, 160)
        self.meta_labels["file_path"].setText(elided)
        self.meta_labels["file_path"].setToolTip(norm)

        self.meta_labels["format"].setText(model.format)
        self.meta_labels["vertices"].setText(f"{model.num_vertices:,}")
        
        is_gcode = hasattr(model, 'lines') and len(model.lines) > 0
        if is_gcode:
            self.meta_group.setTitle("Toolpath Details")
            self.meta_row_labels["triangles"].setText("Lines:")
            self.meta_row_labels["volume"].setText("Est. Volume:")
            self.meta_labels["triangles"].setText(f"{model.num_lines:,}")
            
            # Format sizes nicely in mm
            sz = model.bounding_box_size
            self.meta_labels["bounds"].setText(f"{sz[0]:.1f} x {sz[1]:.1f} x {sz[2]:.1f} mm")
            self.meta_labels["volume"].setText("N/A (Toolpath)")
        else:
            self.meta_group.setTitle("Mesh Details")
            self.meta_row_labels["triangles"].setText("Triangles:")
            self.meta_row_labels["volume"].setText("Est. Volume:")
            self.meta_labels["triangles"].setText(f"{model.num_triangles:,}")
            
            # Format sizes nicely in mm
            sz = model.bounding_box_size
            self.meta_labels["bounds"].setText(f"{sz[0]:.1f} x {sz[1]:.1f} x {sz[2]:.1f} mm")
            
            # Volume formatting (cubic millimeters converted to cubic centimeters)
            vol_cc = model.volume / 1000.0
            self.meta_labels["volume"].setText(f"{vol_cc:,.2f} cm³ ({model.volume:,.1f} mm³)")

    def _reset_metadata_labels(self):
        """Clear details panel text during loading state."""
        self.meta_row_labels["triangles"].setText("Triangles:")
        self.meta_row_labels["volume"].setText("Est. Volume:")
        for key, val_label in self.meta_labels.items():
            val_label.setText("Loading...")
            if key == "file_path":
                val_label.setToolTip("")

    def _toggle_image_visibility(self, state):
        """Toggle media files visibility in the file browser model."""
        is_checked = self.show_images_cb.isChecked()
        base_filters = ["*.stl", "*.3mf", "*.obj", "*.gcode", "*.gco"]
        media_filters = ["*.jpg", "*.jpeg", "*.png", "*.gif", "*.tiff", "*.tif", "*.svg", "*.bmp", "*.webm"]
        
        if is_checked:
            self.file_model.setNameFilters(base_filters + media_filters)
            self.status_bar.showMessage("Image & Media files enabled in browser.")
        else:
            self.file_model.setNameFilters(base_filters)
            self.status_bar.showMessage("Image & Media files filtered out.")
            
            # If the currently selected file is a media file, clear/reset the viewport
            indexes = self.tree_view.selectionModel().selectedIndexes()
            if indexes:
                filepath = self.file_model.filePath(indexes[0])
                ext = os.path.splitext(filepath)[1].lower()
                media_exts = ['.jpg', '.jpeg', '.png', '.gif', '.tiff', '.tif', '.svg', '.bmp', '.webm']
                if ext in media_exts:
                    # Switch viewport stack back to 3D page
                    self.viewport_stack.setCurrentIndex(0)
                    self._cleanup_media_playback()
                    self._reset_metadata_labels()
                    self.tree_view.clearSelection()
                    self.status_bar.showMessage("Active media cleared.")
                    
        # If the currently selected index is a directory, reload it to update grid!
        indexes = self.tree_view.selectionModel().selectedIndexes()
        if indexes:
            filepath = self.file_model.filePath(indexes[0])
            if os.path.isdir(filepath):
                self._file_selected(self.tree_view.selectionModel().selection(), None)

    def _cleanup_media_playback(self):
        """Stop GIF movies and multimedia players to release resources."""
        self.current_pixmap = None
        self.current_media_type = None
        self._img_drag_active = False
        self.image_viewer_area.viewport().setCursor(Qt.ArrowCursor)
        if hasattr(self, 'current_movie') and self.current_movie:
            self.current_movie.stop()
            self.current_movie = None
            self.image_label.setMovie(None)
            
        if hasattr(self, 'media_player') and self.media_player:
            self.media_player.stop()
            self.play_btn.setText("Play")
            self.seeker.setValue(0)
            self.time_label.setText("00:00 / 00:00")

    def _load_media_file(self, filepath, ext):
        """Directly parse and load media files inside the StackedWidget."""
        basename = os.path.basename(filepath)
        self.status_bar.showMessage(f"Reading: {basename} ...")
        self._reset_metadata_labels()
        self._current_filepath = filepath

        # File size + path (shared across all media types)
        file_size_bytes = os.path.getsize(filepath)
        size_str = self._format_file_size(file_size_bytes)
        self.meta_labels["file_size"].setText(size_str)
        norm = os.path.normpath(filepath)
        fm = self.meta_labels["file_path"].fontMetrics()
        self.meta_labels["file_path"].setText(fm.elidedText(norm, Qt.ElideLeft, 160))
        self.meta_labels["file_path"].setToolTip(norm)
        
        # Hide standard image label and SVG widget first, then show appropriate one
        self.image_label.hide()
        self.svg_widget.hide()
        
        if ext in ['.jpg', '.jpeg', '.png', '.tiff', '.tif', '.bmp']:
            # --- STATIC IMAGES ---
            self.viewport_stack.setCurrentIndex(1)
            self.image_label.show()

            pixmap = QPixmap(filepath)
            if not pixmap.isNull():
                self.current_pixmap = pixmap
                self.current_media_type = 'image'
                aw = self.image_viewer_area.width() - 4
                ah = self.image_viewer_area.height() - 4
                self.image_zoom = min(aw / pixmap.width(), ah / pixmap.height())
                self._zoom_image(1.0)

                self.meta_group.setTitle("Image Details")
                self.meta_row_labels["volume"].setText("File Size:")
                self.meta_labels["file_name"].setText(os.path.basename(filepath))
                self.meta_labels["format"].setText(ext[1:].upper())
                self.meta_labels["vertices"].setText("N/A")
                self.meta_labels["triangles"].setText("N/A")
                self.meta_labels["bounds"].setText(f"{pixmap.width()} x {pixmap.height()} px")
                self.meta_labels["volume"].setText(size_str)
                self.status_bar.showMessage(
                    f"Image: {basename}  —  {ext[1:].upper()}  —  {pixmap.width()} × {pixmap.height()} px  —  {size_str}"
                )
            else:
                self.status_bar.showMessage("Error: Failed to load image file.")

        elif ext == '.gif':
            # --- ANIMATED GIFS ---
            self.viewport_stack.setCurrentIndex(1)
            self.image_label.show()

            self.current_movie = QMovie(filepath)
            self.current_media_type = 'gif'
            self.current_movie.jumpToFrame(0)
            native_px = self.current_movie.currentPixmap()
            from PySide6.QtCore import QSize
            if not native_px.isNull():
                self._gif_native_size = QSize(native_px.width(), native_px.height())
                aw = self.image_viewer_area.width() - 4
                ah = self.image_viewer_area.height() - 4
                self.image_zoom = min(aw / native_px.width(), ah / native_px.height())
            else:
                self._gif_native_size = QSize()
                self.image_zoom = 1.0
            self.image_label.setMovie(self.current_movie)
            self._zoom_image(1.0)
            self.current_movie.start()

            w = native_px.width() if not native_px.isNull() else "Unknown"
            h = native_px.height() if not native_px.isNull() else "Unknown"

            self.meta_group.setTitle("Image Details (GIF)")
            self.meta_row_labels["volume"].setText("File Size:")
            self.meta_labels["file_name"].setText(os.path.basename(filepath))
            self.meta_labels["format"].setText("GIF (Animated)")
            self.meta_labels["vertices"].setText("N/A")
            self.meta_labels["triangles"].setText("N/A")
            self.meta_labels["bounds"].setText(f"{w} x {h} px")
            self.meta_labels["volume"].setText(size_str)
            self.status_bar.showMessage(
                f"GIF: {basename}  —  Animated  —  {w} × {h} px  —  {size_str}"
            )
            
        elif ext == '.svg':
            # --- VECTOR SVGS ---
            self.viewport_stack.setCurrentIndex(1)
            self.svg_widget.show()

            self.svg_widget.load(filepath)
            self.current_media_type = 'svg'
            svg_size = self.svg_widget.renderer().defaultSize()
            self._svg_native_size = svg_size
            if svg_size.width() > 0 and svg_size.height() > 0:
                aw = self.image_viewer_area.width() - 4
                ah = self.image_viewer_area.height() - 4
                self.image_zoom = min(aw / svg_size.width(), ah / svg_size.height())
            else:
                self.image_zoom = 1.0
            self._zoom_image(1.0)
            
            # Bind vector metadata
            self.meta_group.setTitle("Vector Details")
            self.meta_row_labels["volume"].setText("File Size:")
            self.meta_labels["file_name"].setText(os.path.basename(filepath))
            self.meta_labels["format"].setText("SVG (Vector)")
            self.meta_labels["vertices"].setText("N/A")
            self.meta_labels["triangles"].setText("N/A")
            self.meta_labels["bounds"].setText(f"{svg_size.width()} x {svg_size.height()} px")
            self.meta_labels["volume"].setText(size_str)
            self.status_bar.showMessage(
                f"SVG: {basename}  —  Vector  —  {svg_size.width()} × {svg_size.height()} px  —  {size_str}"
            )
            
        elif ext == '.webm':
            # --- WEBM VIDEOS ---
            self.viewport_stack.setCurrentIndex(2)
            
            # Set media source
            file_url = QUrl.fromLocalFile(filepath)
            self.media_player.setSource(file_url)
            
            # Automatically start video playback
            self.media_player.play()
            self.play_btn.setText("Pause")
            
            # Bind video metadata
            self.meta_group.setTitle("Video Details")
            self.meta_row_labels["volume"].setText("File Size:")
            self.meta_labels["file_name"].setText(os.path.basename(filepath))
            self.meta_labels["format"].setText("WebM (Video)")
            self.meta_labels["vertices"].setText("N/A")
            self.meta_labels["triangles"].setText("WebM Video Stream")
            self.meta_labels["bounds"].setText("N/A")
            self.meta_labels["volume"].setText(size_str)
            self.status_bar.showMessage(
                f"Video: {basename}  —  WebM  —  {size_str}"
            )

    def _toggle_video_playback(self):
        if self.media_player.playbackState() == QMediaPlayer.PlayingState:
            self.media_player.pause()
            self.play_btn.setText("Play")
        else:
            self.media_player.play()
            self.play_btn.setText("Pause")
            
    def _set_video_position(self, position):
        self.media_player.setPosition(position)
        
    def _toggle_video_mute(self):
        is_muted = self.audio_output.isMuted()
        self.audio_output.setMuted(not is_muted)
        self.mute_btn.setText("Unmute" if not is_muted else "Mute")
        
    def _video_position_changed(self, position):
        if not self.seeker.isSliderDown():
            self.seeker.setValue(position)
        self._update_video_time_label(position, self.media_player.duration())
        
    def _video_duration_changed(self, duration):
        self.seeker.setRange(0, duration)
        self._update_video_time_label(self.media_player.position(), duration)
        
    def _update_video_time_label(self, position, duration):
        pos_sec = position // 1000
        dur_sec = duration // 1000
        
        pos_min = pos_sec // 60
        pos_sec = pos_sec % 60
        
        dur_min = dur_sec // 60
        dur_sec = dur_sec % 60
        
        self.time_label.setText(f"{pos_min:02d}:{pos_sec:02d} / {dur_min:02d}:{dur_sec:02d}")

    def _format_file_size(self, size_bytes):
        """Format raw byte counts into human readable strings."""
        if size_bytes < 1024:
            return f"{size_bytes} Bytes"
        elif size_bytes < 1024 * 1024:
            return f"{size_bytes / 1024:.1f} KB"
        else:
            return f"{size_bytes / (1024 * 1024):.2f} MB"

    def _shading_changed(self, idx):
        """Relay shading mode modifications to the renderer."""
        preset_key = self.shading_combo.itemData(idx) or 'smooth'
        self.vtk_widget.set_shading_mode(preset_key)
        
    def _select_color(self):
        """Trigger QColorDialog to pick model diffuse color."""
        # Convert Normalized VTK active color back to QColor
        r, g, b = self.vtk_widget.current_color
        current_qcolor = QColor(int(r*255), int(g*255), int(b*255))
        
        color = QColorDialog.getColor(current_qcolor, self, "Select Model Color")
        if color.isValid():
            self.vtk_widget.set_model_color(color)
            

        
    def _rotate_changed(self, state):
        """Toggle viewport continuous camera orbit rotation timer."""
        self.vtk_widget.toggle_auto_rotate(self.rotate_cb.isChecked())

    def eventFilter(self, obj, event):
        if obj == self.image_viewer_area.viewport() and self.viewport_stack.currentIndex() == 1:
            if event.type() == QEvent.Wheel:
                delta = event.angleDelta().y()
                self._zoom_image(1.15 if delta > 0 else 1 / 1.15)
                return True
            elif event.type() == QEvent.MouseButtonPress and event.button() == Qt.LeftButton:
                self._img_drag_active = True
                self._img_drag_start_pos = event.globalPosition().toPoint()
                self._img_drag_scroll_start = (
                    self.image_viewer_area.horizontalScrollBar().value(),
                    self.image_viewer_area.verticalScrollBar().value(),
                )
                self.image_viewer_area.viewport().setCursor(Qt.ClosedHandCursor)
                return True
            elif event.type() == QEvent.MouseMove and self._img_drag_active:
                delta = event.globalPosition().toPoint() - self._img_drag_start_pos
                self.image_viewer_area.horizontalScrollBar().setValue(
                    self._img_drag_scroll_start[0] - delta.x()
                )
                self.image_viewer_area.verticalScrollBar().setValue(
                    self._img_drag_scroll_start[1] - delta.y()
                )
                return True
            elif event.type() == QEvent.MouseButtonRelease and event.button() == Qt.LeftButton:
                self._img_drag_active = False
                self.image_viewer_area.viewport().setCursor(Qt.ArrowCursor)
                return True
        return super().eventFilter(obj, event)

    def _zoom_image(self, factor):
        """Apply a multiplicative zoom factor to the current image/gif/svg."""
        from PySide6.QtCore import QSize
        self.image_zoom = max(0.05, min(20.0, self.image_zoom * factor))
        if self.current_media_type == 'image' and self.current_pixmap:
            w = max(1, int(self.current_pixmap.width() * self.image_zoom))
            h = max(1, int(self.current_pixmap.height() * self.image_zoom))
            scaled = self.current_pixmap.scaled(w, h, Qt.KeepAspectRatio, Qt.SmoothTransformation)
            self.image_label.setPixmap(scaled)
            self.image_label.resize(scaled.size())
        elif self.current_media_type == 'gif' and hasattr(self, 'current_movie') and self.current_movie:
            native = getattr(self, '_gif_native_size', None)
            if native and native.isValid():
                nw = max(1, int(native.width() * self.image_zoom))
                nh = max(1, int(native.height() * self.image_zoom))
                self.current_movie.setScaledSize(QSize(nw, nh))
                self.image_label.resize(nw, nh)
        elif self.current_media_type == 'svg':
            native = getattr(self, '_svg_native_size', None)
            if native and native.isValid():
                self.svg_widget.setFixedSize(
                    max(1, int(native.width() * self.image_zoom)),
                    max(1, int(native.height() * self.image_zoom))
                )
        self.image_container.adjustSize()

    def _sibling_files(self):
        """Return sorted list of supported mesh/media files in the same directory as the current file."""
        indexes = self.tree_view.selectionModel().selectedIndexes()
        if not indexes:
            return [], -1
        current_path = os.path.abspath(self.file_model.filePath(indexes[0]))
        if os.path.isdir(current_path):
            return [], -1
        mesh_exts = ['.stl', '.3mf', '.obj', '.gcode', '.gco']
        media_exts = ['.jpg', '.jpeg', '.png', '.gif', '.tiff', '.tif', '.svg', '.bmp', '.webm']
        folder = os.path.dirname(current_path)
        try:
            entries = [
                os.path.join(folder, e.name)
                for e in os.scandir(folder)
                if e.is_file() and os.path.splitext(e.name)[1].lower() in mesh_exts + media_exts
            ]
        except Exception:
            return [], -1
        entries.sort(key=lambda x: os.path.basename(x).lower())
        try:
            idx = next(i for i, p in enumerate(entries) if os.path.abspath(p) == current_path)
        except StopIteration:
            idx = -1
        return entries, idx

    def _load_prev_file(self):
        files, idx = self._sibling_files()
        if idx > 0:
            self._select_file_in_tree(files[idx - 1])

    def _load_next_file(self):
        files, idx = self._sibling_files()
        if 0 <= idx < len(files) - 1:
            self._select_file_in_tree(files[idx + 1])

    def _select_file_in_tree(self, filepath):
        index = self.file_model.index(filepath)
        if index.isValid():
            from PySide6.QtCore import QItemSelectionModel
            self.tree_view.setCurrentIndex(index)
            self.tree_view.selectionModel().select(index, QItemSelectionModel.Select)

    def _precache_directory_thumbnails(self, directory):
        """Scan folder for uncached 3D models and pre-cache thumbnails in the background."""
        if not directory or not os.path.isdir(directory):
            return

        mesh_exts = ['.stl', '.3mf', '.obj', '.gcode', '.gco']
        uncached_3d = []
        cache = get_thumb_cache()

        try:
            for entry in os.scandir(directory):
                try:
                    if entry.is_file():
                        ext = os.path.splitext(entry.name)[1].lower()
                        if ext in mesh_exts and not cache.is_valid(entry.path):
                            uncached_3d.append(entry.path)
                except Exception as fe:
                    print(f"Skipping pre-cache scan entry: {fe}")
        except Exception as e:
            print(f"Error scanning directory for pre-caching: {e}")
            return

        if uncached_3d:
            if hasattr(self, 'precache_worker') and self.precache_worker and self.precache_worker.isRunning():
                self.precache_worker.stop()
                self.precache_worker.wait()
            self.precache_worker = ThumbnailGeneratorWorker(uncached_3d)
            self.precache_worker.model_loaded.connect(self._on_precache_model_loaded)
            self.precache_worker.start()

    def _on_precache_model_loaded(self, filepath, model_data):
        self._precache_render_queue.append((filepath, model_data))
        if not self._precache_render_timer.isActive():
            self._precache_render_timer.start(0)

    def _precache_render_next(self):
        if not self._precache_render_queue:
            return
        filepath, model_data = self._precache_render_queue.popleft()
        try:
            png_bytes = generate_vtk_thumbnail(model_data)
            if png_bytes:
                cache = get_thumb_cache()
                cache.put(filepath, png_bytes)
                pixmap = cache.get_pixmap(filepath)
                if pixmap and hasattr(self, 'thumbnail_grid') and self.thumbnail_grid:
                    self.thumbnail_grid._on_thumbnail_ready(filepath, pixmap)
        except Exception as e:
            print(f"Error rendering pre-cached thumbnail for {filepath}: {e}")
        if self._precache_render_queue:
            self._precache_render_timer.start(0)

    def closeEvent(self, event):
        """Clean up background workers and interactor when closing."""
        if hasattr(self, '_precache_render_timer'):
            self._precache_render_timer.stop()
        if hasattr(self, '_precache_render_queue'):
            self._precache_render_queue.clear()
        if hasattr(self, 'thumbnail_grid') and self.thumbnail_grid:
            if hasattr(self.thumbnail_grid, '_stop_workers'):
                self.thumbnail_grid._stop_workers()
        if hasattr(self, 'precache_worker') and self.precache_worker and self.precache_worker.isRunning():
            self.precache_worker.stop()
            self.precache_worker.wait()
        if hasattr(self, 'active_worker') and self.active_worker and self.active_worker.isRunning():
            self.active_worker.terminate()
            self.active_worker.wait()
        get_thumb_cache().close()
        event.accept()


class QFrameSystemModel(QFileSystemModel):
    """Custom FileSystemModel that overrides icons and filters directories elegantly."""
    
    def data(self, index, role=Qt.DisplayRole):
        # We can customize icons or names here if needed.
        return super().data(index, role)


def main():
    # Set high-resolution display scaling
    os.environ["QT_AUTO_SCREEN_SCALE_FACTOR"] = "1"
    
    # Run the PySide6 Application loop
    app = QApplication(sys.argv)
    
    window = MainWindow()
    window.show()
    sys.exit(app.exec())

if __name__ == "__main__":
    main()
