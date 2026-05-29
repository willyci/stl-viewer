import os
import sys

from PySide6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QVBoxLayout, QHBoxLayout,
    QPushButton, QLineEdit, QScrollArea, QStatusBar, QFileDialog,
    QLabel, QCheckBox, QFrame,
)
from PySide6.QtCore import Qt

from app import ThumbnailGridWidget, get_thumb_cache
from styles import DARK_THEME_QSS

_BTN = (
    "QPushButton { background: #1A1D26; border: 1px solid #2A2F3D; border-radius: 4px;"
    " color: #E2E8F0; font-size: 11px; padding: 4px 10px; }"
    "QPushButton:hover { background: #232836; border-color: #00F0FF; color: #00F0FF; }"
    "QPushButton:pressed { background: #0D0F16; }"
)


class ThumbnailBrowserWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("Apex Thumbnail Browser")
        self.resize(1280, 820)
        self.setStyleSheet(DARK_THEME_QSS)
        self._current_path = "C:\\"
        self._init_ui()
        self._navigate_to(self._current_path)

    # ── UI setup ──────────────────────────────────────────────────────────────

    def _init_ui(self):
        # Status bar first so signals can connect to it
        self.status_bar = QStatusBar()
        self.status_bar.setStyleSheet(
            "QStatusBar { background: #0A0C10; color: #A4ADC4;"
            " border-top: 1px solid #1F2432; font-size: 11px; padding: 2px 8px; }"
        )
        self.setStatusBar(self.status_bar)

        central = QWidget()
        self.setCentralWidget(central)
        root = QVBoxLayout(central)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)

        root.addWidget(self._build_toolbar())

        # Thumbnail scroll area
        self.scroll_area = QScrollArea()
        self.scroll_area.setWidgetResizable(True)
        self.scroll_area.setStyleSheet("QScrollArea { background: #0A0C10; border: none; }")

        self.thumbnail_grid = ThumbnailGridWidget()
        self.scroll_area.setWidget(self.thumbnail_grid)
        self.thumbnail_grid.status_message.connect(self.status_bar.showMessage)
        self.thumbnail_grid.card_clicked.connect(self._on_card_clicked)
        root.addWidget(self.scroll_area, stretch=1)

    def _build_toolbar(self):
        bar = QFrame()
        bar.setFixedHeight(50)
        bar.setStyleSheet(
            "QFrame { background: #11131A; border-bottom: 1px solid #1E222D; }"
        )
        tb = QHBoxLayout(bar)
        tb.setContentsMargins(12, 8, 12, 8)
        tb.setSpacing(8)

        logo = QLabel("⚡")
        logo.setStyleSheet("font-size: 18px; color: #00F0FF; background: transparent;")
        tb.addWidget(logo)

        title = QLabel("Thumbnail Browser")
        title.setStyleSheet(
            "font-size: 13px; font-weight: bold; color: #E2E8F0; background: transparent;"
        )
        tb.addWidget(title)

        sep = QFrame()
        sep.setFrameShape(QFrame.VLine)
        sep.setStyleSheet("color: #232836;")
        tb.addWidget(sep)

        self.path_input = QLineEdit()
        self.path_input.setPlaceholderText("Enter folder path ...")
        self.path_input.setStyleSheet(
            "QLineEdit { color: #E2E8F0; font-size: 11px; font-weight: bold;"
            " background: #0D0F16; border: 1px solid #1E222D; border-radius: 4px;"
            " padding: 4px 8px; }"
            "QLineEdit:focus { border-color: #00F0FF; }"
        )
        self.path_input.returnPressed.connect(self._on_path_return)
        tb.addWidget(self.path_input, stretch=1)

        up_btn = QPushButton("↑")
        up_btn.setFixedWidth(32)
        up_btn.setToolTip("Go up one folder level")
        up_btn.setStyleSheet(_BTN + " QPushButton { font-size: 15px; padding: 4px 4px; }")
        up_btn.clicked.connect(self._navigate_up)
        tb.addWidget(up_btn)

        browse_btn = QPushButton("Browse ...")
        browse_btn.setStyleSheet(_BTN)
        browse_btn.clicked.connect(self._browse)
        tb.addWidget(browse_btn)

        sep2 = QFrame()
        sep2.setFrameShape(QFrame.VLine)
        sep2.setStyleSheet("color: #232836;")
        tb.addWidget(sep2)

        self.show_media_cb = QCheckBox("Images & Media")
        self.show_media_cb.setChecked(False)
        self.show_media_cb.setStyleSheet(
            "QCheckBox { color: #A4ADC4; font-size: 11px; background: transparent; }"
            "QCheckBox::indicator { width: 13px; height: 13px; }"
        )
        self.show_media_cb.stateChanged.connect(self._refresh)
        tb.addWidget(self.show_media_cb)

        return bar

    # ── Navigation ────────────────────────────────────────────────────────────

    def _navigate_to(self, path):
        path = os.path.normpath(path)
        if not os.path.isdir(path):
            self.status_bar.showMessage(f"Invalid path: {path}")
            return
        self._current_path = path
        self.path_input.setText(path)
        self._load_folder(path)

    def _navigate_up(self):
        parent = os.path.dirname(self._current_path)
        if parent and parent != self._current_path:
            self._navigate_to(parent)

    def _browse(self):
        selected = QFileDialog.getExistingDirectory(
            self, "Select Folder", self._current_path
        )
        if selected:
            self._navigate_to(selected)

    def _on_path_return(self):
        self._navigate_to(self.path_input.text().strip())

    def _refresh(self):
        self._load_folder(self._current_path)

    # ── Folder loading ────────────────────────────────────────────────────────

    def _load_folder(self, path):
        mesh_exts = {'.stl', '.3mf', '.obj', '.gcode', '.gco'}
        media_exts = {'.jpg', '.jpeg', '.png', '.bmp', '.tiff', '.tif',
                      '.gif', '.svg', '.webm'}
        allowed = mesh_exts | (media_exts if self.show_media_cb.isChecked() else set())

        files = []
        total_bytes = 0
        try:
            for entry in os.scandir(path):
                try:
                    if entry.is_file():
                        ext = os.path.splitext(entry.name)[1].lower()
                        if ext in allowed:
                            files.append(entry.path)
                            total_bytes += entry.stat().st_size
                except OSError:
                    pass
        except PermissionError:
            self.status_bar.showMessage(f"Permission denied: {path}")
            return
        except Exception as e:
            self.status_bar.showMessage(f"Error reading folder: {e}")
            return

        files.sort(key=lambda x: os.path.basename(x).lower())
        self.thumbnail_grid.set_files(files)

        folder_name = os.path.basename(path) or path
        self.status_bar.showMessage(
            f"Folder: {folder_name}  —  {len(files)} files  —  {self._fmt_size(total_bytes)}"
        )

    # ── Card interaction ──────────────────────────────────────────────────────

    def _on_card_clicked(self, filepath):
        try:
            os.startfile(os.path.normpath(filepath))
            self.status_bar.showMessage(f"Opened: {os.path.basename(filepath)}")
        except Exception as e:
            self.status_bar.showMessage(f"Could not open file: {e}")

    # ── Helpers ───────────────────────────────────────────────────────────────

    @staticmethod
    def _fmt_size(n):
        if n < 1024:
            return f"{n} B"
        if n < 1024 ** 2:
            return f"{n / 1024:.1f} KB"
        if n < 1024 ** 3:
            return f"{n / 1024 ** 2:.2f} MB"
        return f"{n / 1024 ** 3:.2f} GB"

    def closeEvent(self, event):
        self.thumbnail_grid._stop_workers()
        get_thumb_cache().close()
        event.accept()


def main():
    os.environ["QT_AUTO_SCREEN_SCALE_FACTOR"] = "1"
    app = QApplication(sys.argv)
    window = ThumbnailBrowserWindow()
    window.show()
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
