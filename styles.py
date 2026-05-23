# Sleek, premium Slate Dark theme styling with Cyan/Teal accents for PyQt6/PySide6.

DARK_THEME_QSS = """
QMainWindow {
    background-color: #0E1015;
}

/* Glassmorphism sidebar look */
QFrame#SidebarFrame {
    background-color: #151821;
    border-right: 1px solid #232836;
}

QFrame#RightControlDeck {
    background-color: #151821;
    border-left: 1px solid #232836;
}

/* Customized Layout Splitter */
QSplitter::handle {
    background-color: #1F2432;
}
QSplitter::handle:horizontal {
    width: 4px;
}
QSplitter::handle:vertical {
    height: 4px;
}

/* File browser tree */
QTreeView {
    background-color: #11131A;
    border: 1px solid #1E222D;
    border-radius: 6px;
    color: #E2E6F0;
    font-size: 13px;
    padding: 5px;
    outline: 0;
}
QTreeView::item {
    padding: 6px;
    border-radius: 4px;
}
QTreeView::item:hover {
    background-color: #1C202D;
    color: #FFFFFF;
}
QTreeView::item:selected {
    background-color: #163640;
    color: #00F0FF;
    border-left: 3px solid #00F0FF;
    font-weight: bold;
}
QTreeView::branch:has-children:!has-depth:closed {
    image: url(no_image); /* Clears default expander */
}

/* Scrollbars inside file tree */
QTreeView QScrollBar:vertical {
    background: #1A1D28;
    width: 10px;
    margin: 0px;
    border-radius: 5px;
}
QTreeView QScrollBar::handle:vertical {
    background: #4A5578;
    border-radius: 5px;
    min-height: 24px;
}
QTreeView QScrollBar::handle:vertical:hover {
    background: #00F0FF;
}
QTreeView QScrollBar::add-line:vertical,
QTreeView QScrollBar::sub-line:vertical {
    height: 0px;
}
QTreeView QScrollBar::add-page:vertical,
QTreeView QScrollBar::sub-page:vertical {
    background: none;
}
QTreeView QScrollBar:horizontal {
    background: #1A1D28;
    height: 10px;
    margin: 0px;
    border-radius: 5px;
}
QTreeView QScrollBar::handle:horizontal {
    background: #4A5578;
    border-radius: 5px;
    min-width: 24px;
}
QTreeView QScrollBar::handle:horizontal:hover {
    background: #00F0FF;
}
QTreeView QScrollBar::add-line:horizontal,
QTreeView QScrollBar::sub-line:horizontal {
    width: 0px;
}
QTreeView QScrollBar::add-page:horizontal,
QTreeView QScrollBar::sub-page:horizontal {
    background: none;
}

/* Group container titles */
QGroupBox {
    color: #00F0FF;
    font-weight: bold;
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 1px;
    border: 1px solid #232836;
    border-radius: 8px;
    margin-top: 15px;
    padding-top: 15px;
    background-color: #11131A;
}
QGroupBox::title {
    subcontrol-origin: margin;
    subcontrol-position: top left;
    left: 12px;
    padding: 0 5px;
    background-color: #11131A;
}

/* Clean text and labels */
QLabel {
    color: #A4ADC4;
    font-size: 13px;
}
QLabel#HeaderLabel {
    color: #FFFFFF;
    font-size: 16px;
    font-weight: bold;
}
QLabel#ValueLabel {
    color: #FFFFFF;
    font-weight: bold;
}

/* Buttons with modern rounded designs */
QPushButton {
    background-color: #1E2333;
    color: #E2E6F0;
    border: 1px solid #2A3147;
    border-radius: 6px;
    padding: 8px 16px;
    font-size: 12px;
    font-weight: bold;
}
QPushButton:hover {
    background-color: #293147;
    border-color: #00F0FF;
    color: #FFFFFF;
}
QPushButton:pressed {
    background-color: #151A26;
    border-color: #00BCCF;
}

/* Color Accent Buttons */
QPushButton#AccentButton {
    background-color: #00BCCF;
    color: #0E1015;
    border: none;
}
QPushButton#AccentButton:hover {
    background-color: #00F0FF;
    color: #0E1015;
}
QPushButton#AccentButton:pressed {
    background-color: #0093A3;
}

/* Table Widget for mesh metadata */
QTableWidget {
    background-color: #11131A;
    border: 1px solid #232836;
    border-radius: 6px;
    gridline-color: #1E222D;
    color: #D2D8E6;
}
QTableWidget::item {
    padding: 6px;
}
QTableWidget::item:selected {
    background-color: #1C202D;
    color: #FFFFFF;
}
QHeaderView::section {
    background-color: #181B26;
    color: #00F0FF;
    padding: 6px;
    border: none;
    border-bottom: 1px solid #232836;
    font-weight: bold;
    font-size: 11px;
    text-transform: uppercase;
}

/* ComboBox for shade selection */
QComboBox {
    background-color: #1E2333;
    border: 1px solid #2A3147;
    border-radius: 6px;
    padding: 6px 12px;
    color: #E2E6F0;
    font-size: 12px;
}
QComboBox:hover {
    border-color: #00F0FF;
}
QComboBox::drop-down {
    border: none;
    width: 20px;
}
QComboBox QAbstractItemView {
    background-color: #181B26;
    border: 1px solid #2A3147;
    selection-background-color: #293147;
    selection-color: #00F0FF;
    color: #D2D8E6;
}

/* Custom checkbox elements */
QCheckBox {
    color: #A4ADC4;
    font-size: 12px;
    spacing: 8px;
}
QCheckBox::indicator {
    width: 16px;
    height: 16px;
    border: 1px solid #2A3147;
    border-radius: 4px;
    background-color: #1E2333;
}
QCheckBox::indicator:hover {
    border-color: #00F0FF;
}
QCheckBox::indicator:checked {
    background-color: #00BCCF;
    border-color: #00F0FF;
}
"""
