import math
from PySide6.QtWidgets import QWidget, QVBoxLayout
from PySide6.QtCore import QTimer
import numpy as np
from vtkmodules.vtkCommonCore import vtkPoints
from vtkmodules.vtkCommonDataModel import vtkPolyData, vtkCellArray, vtkLine
from vtkmodules.vtkFiltersCore import vtkPolyDataNormals
from vtkmodules.vtkRenderingCore import vtkRenderer, vtkActor, vtkPolyDataMapper, vtkLight
from vtkmodules.vtkRenderingOpenGL2 import vtkOpenGLRenderer  # noqa: F401 – registers OpenGL backend
from vtkmodules.vtkInteractionStyle import vtkInteractorStyleTrackballCamera
from vtkmodules.vtkInteractionWidgets import vtkOrientationMarkerWidget
from vtkmodules.vtkRenderingAnnotation import vtkAxesActor
from vtkmodules.qt.QVTKRenderWindowInteractor import QVTKRenderWindowInteractor
from vtkmodules.util import numpy_support

class VTKRendererWidget(QWidget):
    """Custom VTK 3D Render Widget integrated with PySide6."""
    
    def __init__(self, parent=None):
        super().__init__(parent)
        self.layout = QVBoxLayout(self)
        self.layout.setContentsMargins(0, 0, 0, 0)
        
        # Initialize VTK widget
        self.vtkWidget = QVTKRenderWindowInteractor(self)
        self.layout.addWidget(self.vtkWidget)
        
        # Create VTK rendering pipeline
        self.renderer = vtkRenderer()
        self.vtkWidget.GetRenderWindow().AddRenderer(self.renderer)
        self.interactor = self.vtkWidget.GetRenderWindow().GetInteractor()
        
        # Trackball Camera Style is the gold standard for CAD viewers
        style = vtkInteractorStyleTrackballCamera()
        self.interactor.SetInteractorStyle(style)
        
        # Scene references
        self.actor = None
        self.grid_actor = None
        self.marker_widget = None
        self.is_grid_visible = True
        self.current_color = (0.0, 0.94, 1.0)  # Active Cyan
        self.is_line_mode = False
        
        # Continuous Auto-Rotate timer
        self.rotate_timer = QTimer(self)
        self.rotate_timer.timeout.connect(self._auto_rotate_tick)
        self.rotation_angle = 0.8  # degrees per frame
        
        # Initialize Scene
        self._setup_viewport()
        
    def _setup_viewport(self):
        """Set up beautiful dark gradient environment, lighting, and markers."""
        # 1. Elegant slate dark gradient background
        self.renderer.SetBackground(0.07, 0.08, 0.11)  # Slate dark #12141C
        self.renderer.SetBackground2(0.15, 0.17, 0.22)  # Lighter slate #262B38
        self.renderer.GradientBackgroundOn()
        
        # 2. Premium diffuse + directional lighting rig
        self.renderer.RemoveAllLights()
        
        # Direct Camera Light (moves dynamically with camera orbit)
        cam_light = vtkLight()
        cam_light.SetLightTypeToCameraLight()
        cam_light.SetDiffuseColor(1.0, 1.0, 1.0)
        cam_light.SetSpecularColor(1.0, 1.0, 1.0)
        cam_light.SetIntensity(0.85)
        self.renderer.AddLight(cam_light)
        
        # Stationary Top-Left fill light
        top_light = vtkLight()
        top_light.SetPosition(-1.0, 1.0, 1.0)
        top_light.SetLightTypeToSceneLight()
        top_light.SetDiffuseColor(0.2, 0.25, 0.35)
        top_light.SetIntensity(0.3)
        self.renderer.AddLight(top_light)
        
        # 3. Interactive Corner Orientation Indicator widget
        axes = vtkAxesActor()
        axes.SetTotalLength(1.0, 1.0, 1.0)
        
        # Modern colored labels
        axes.GetXAxisCaptionActor2D().GetCaptionTextProperty().SetColor(1.0, 0.35, 0.35)
        axes.GetYAxisCaptionActor2D().GetCaptionTextProperty().SetColor(0.35, 1.0, 0.35)
        axes.GetZAxisCaptionActor2D().GetCaptionTextProperty().SetColor(0.35, 0.75, 1.0)
        
        self.marker_widget = vtkOrientationMarkerWidget()
        self.marker_widget.SetOrientationMarker(axes)
        self.marker_widget.SetInteractor(self.interactor)
        self.marker_widget.SetViewport(0.0, 0.0, 0.18, 0.18)
        self.marker_widget.SetEnabled(1)
        self.marker_widget.InteractiveOff()  # Prevent user modification
        
        # 4. Generate empty coordinate grid at origin
        self.update_grid()
        
        # 5. Initialize the Interactor
        self.vtkWidget.Initialize()
        self.vtkWidget.Start()
        
    def set_model(self, model_data):
        """Bind mesh vertices and triangles to the VTK PolyData pipelines."""
        if len(model_data.vertices) == 0:
            return
            
        # Stop auto-rotation when a new model loads
        self.toggle_auto_rotate(False)
        
        has_lines = hasattr(model_data, 'lines') and len(model_data.lines) > 0
        self.is_line_mode = has_lines
        
        # 1. Bind vertices array to vtkPoints using hardware-accelerated memory views
        points = vtkPoints()
        vtk_points_data = numpy_support.numpy_to_vtk(model_data.vertices, deep=True)
        points.SetData(vtk_points_data)
        
        poly_data = vtkPolyData()
        poly_data.SetPoints(points)
        
        if has_lines:
            # Build cell index array for line segments (2 vertices per line)
            num_lines = len(model_data.lines)
            cells_flat = np.empty((num_lines, 3), dtype=np.int64)
            cells_flat[:, 0] = 2
            cells_flat[:, 1:] = model_data.lines
            cells_flat = cells_flat.ravel()
            
            cells = vtkCellArray()
            vtk_cells_data = numpy_support.numpy_to_vtkIdTypeArray(cells_flat, deep=True)
            cells.SetCells(num_lines, vtk_cells_data)
            
            poly_data.SetLines(cells)
            
            # Set or update Actor
            if self.actor is None:
                mapper = vtkPolyDataMapper()
                mapper.SetInputData(poly_data)
                
                self.actor = vtkActor()
                self.actor.SetMapper(mapper)
                self.renderer.AddActor(self.actor)
            else:
                self.actor.GetMapper().SetInputData(poly_data)
                
            # Apply dynamic G-Code toolpath visual parameters
            self.actor.GetProperty().SetColor(self.current_color[0], self.current_color[1], self.current_color[2])
            self.actor.GetProperty().LightingOff()  # Disable lighting to avoid errors or dark lines
            self.actor.GetProperty().SetLineWidth(1.5)
            self.actor.GetProperty().SetRepresentationToSurface()
        else:
            # 2. Build cell index array (triangles list) with prefixed cell sizes
            num_triangles = len(model_data.triangles)
            cells_flat = np.empty((num_triangles, 4), dtype=np.int64)
            cells_flat[:, 0] = 3
            cells_flat[:, 1:] = model_data.triangles
            cells_flat = cells_flat.ravel()
            
            cells = vtkCellArray()
            vtk_cells_data = numpy_support.numpy_to_vtkIdTypeArray(cells_flat, deep=True)
            cells.SetCells(num_triangles, vtk_cells_data)
            
            poly_data.SetPolys(cells)
            
            # 4. Filter to compute point normals for soft, professional smooth shading
            normals_filter = vtkPolyDataNormals()
            normals_filter.SetInputData(poly_data)
            normals_filter.ComputePointNormalsOn()
            normals_filter.ComputeCellNormalsOff()
            normals_filter.ConsistencyOn()
            normals_filter.Update()
            
            # 5. Set or update Actor
            if self.actor is None:
                mapper = vtkPolyDataMapper()
                mapper.SetInputConnection(normals_filter.GetOutputPort())
                
                self.actor = vtkActor()
                self.actor.SetMapper(mapper)
                self.renderer.AddActor(self.actor)
            else:
                self.actor.GetMapper().SetInputData(None) # Reset direct input data
                self.actor.GetMapper().SetInputConnection(normals_filter.GetOutputPort())
                
            # 6. Apply visual parameters
            self.actor.GetProperty().SetColor(self.current_color[0], self.current_color[1], self.current_color[2])
            self.actor.GetProperty().LightingOn()
            self.actor.GetProperty().SetInterpolationToPhong()  # Sleek smooth interpolation
            self.actor.GetProperty().SetAmbient(0.18)
            self.actor.GetProperty().SetDiffuse(0.82)
            self.actor.GetProperty().SetSpecular(0.45)
            self.actor.GetProperty().SetSpecularPower(35.0)
            self.actor.GetProperty().SetLineWidth(1.0)
            
        # 7. Relocate visual coordinate grid to sit right beneath model floor
        self.update_grid(model_data.bounds)
        
        # 8. Recenter viewport and redraw
        self.renderer.ResetCamera()
        self.set_view_preset('isometric')
        self.vtkWidget.GetRenderWindow().Render()
        
    def update_grid(self, bounds=None):
        """Draw an elegant perspective grid beneath the model bounds or origin."""
        if self.grid_actor is not None:
            self.renderer.RemoveActor(self.grid_actor)
            self.grid_actor = None
            
        if not self.is_grid_visible:
            self.vtkWidget.GetRenderWindow().Render()
            return
            
        # Determine grid size based on bounding box
        grid_size = 150.0
        z_pos = 0.0
        
        if bounds is not None:
            # Scale grid size dynamically to accommodate large models
            dx = bounds[0][1] - bounds[0][0]
            dy = bounds[1][1] - bounds[1][0]
            grid_size = max(dx, dy) * 2.5
            grid_size = max(grid_size, 10.0)  # Min cap
            
            # Position grid floor just 1% below the lowest vertex to prevent Z-fighting clipping
            z_pos = bounds[2][0] - (bounds[2][1] - bounds[2][0]) * 0.01
            if math.isnan(z_pos) or math.isinf(z_pos):
                z_pos = 0.0
                
        # Draw grid using line actors
        divisions = 24
        step = grid_size / divisions
        
        points = vtkPoints()
        lines = vtkCellArray()
        
        point_idx = 0
        for i in range(divisions + 1):
            offset = -grid_size / 2.0 + i * step
            
            # X-aligned line (varies Y)
            points.InsertNextPoint(-grid_size / 2.0, offset, z_pos)
            points.InsertNextPoint(grid_size / 2.0, offset, z_pos)
            
            line_x = vtkLine()
            line_x.GetPointIds().SetId(0, point_idx)
            line_x.GetPointIds().SetId(1, point_idx + 1)
            lines.InsertNextCell(line_x)
            point_idx += 2
            
            # Y-aligned line (varies X)
            points.InsertNextPoint(offset, -grid_size / 2.0, z_pos)
            points.InsertNextPoint(offset, grid_size / 2.0, z_pos)
            
            line_y = vtkLine()
            line_y.GetPointIds().SetId(0, point_idx)
            line_y.GetPointIds().SetId(1, point_idx + 1)
            lines.InsertNextCell(line_y)
            point_idx += 2
            
        grid_pd = vtkPolyData()
        grid_pd.SetPoints(points)
        grid_pd.SetLines(lines)
        
        mapper = vtkPolyDataMapper()
        mapper.SetInputData(grid_pd)
        
        self.grid_actor = vtkActor()
        self.grid_actor.SetMapper(mapper)
        self.grid_actor.GetProperty().SetColor(0.28, 0.33, 0.44)  # Elegant Slate Blue lines
        self.grid_actor.GetProperty().SetOpacity(0.35)
        self.grid_actor.GetProperty().LightingOff()
        
        self.renderer.AddActor(self.grid_actor)
        self.vtkWidget.GetRenderWindow().Render()
        
    def set_grid_visibility(self, visible):
        """Toggle coordinate ground grid."""
        self.is_grid_visible = visible
        # Redraw
        if self.actor is not None:
            bounds = self.actor.GetBounds()
            # Convert VTK bounds (6 tuple) to ModelData bounds format (3x2 list)
            custom_bounds = [[bounds[0], bounds[1]], [bounds[2], bounds[3]], [bounds[4], bounds[5]]]
            self.update_grid(custom_bounds)
        else:
            self.update_grid(None)
            
    def set_shading_mode(self, mode):
        """Set shading representation: solid-smooth, solid-flat, wireframe, or point cloud."""
        if self.actor is None:
            return
            
        prop = self.actor.GetProperty()
        if getattr(self, 'is_line_mode', False):
            if mode == 'points':
                prop.SetRepresentationToPoints()
                prop.SetPointSize(3.0)
            else:
                prop.SetRepresentationToSurface()
                prop.SetLineWidth(1.5)
            prop.LightingOff()
        else:
            prop.LightingOn()
            if mode == 'wireframe':
                prop.SetRepresentationToWireframe()
                prop.SetPointSize(1.0)
            elif mode == 'points':
                prop.SetRepresentationToPoints()
                prop.SetPointSize(3.0)
            elif mode == 'flat':
                prop.SetRepresentationToSurface()
                prop.SetInterpolationToFlat()
            elif mode == 'smooth':
                prop.SetRepresentationToSurface()
                prop.SetInterpolationToPhong()
            
        self.vtkWidget.GetRenderWindow().Render()
        
    def set_model_color(self, qcolor):
        """Set model visual diffuse color."""
        # Convert QColor to VTK Normalized Floats
        r = qcolor.red() / 255.0
        g = qcolor.green() / 255.0
        b = qcolor.blue() / 255.0
        self.current_color = (r, g, b)
        
        if self.actor is not None:
            self.actor.GetProperty().SetColor(r, g, b)
            self.vtkWidget.GetRenderWindow().Render()
            
    def set_view_preset(self, view):
        """Realign the camera to preset orientations relative to model centroid."""
        if self.actor is None:
            return
            
        camera = self.renderer.GetActiveCamera()
        bounds = self.actor.GetBounds()
        
        # Model Center
        cx = (bounds[0] + bounds[1]) / 2.0
        cy = (bounds[2] + bounds[3]) / 2.0
        cz = (bounds[4] + bounds[5]) / 2.0
        
        # Calculate camera orbit distance dynamically
        dx = bounds[1] - bounds[0]
        dy = bounds[3] - bounds[2]
        dz = bounds[5] - bounds[4]
        max_dim = max(dx, dy, dz)
        distance = max_dim * 1.85 if max_dim > 0 else 100.0
        
        camera.SetFocalPoint(cx, cy, cz)
        
        if view == 'isometric':
            camera.SetPosition(cx + distance * 0.707, cy - distance * 0.707, cz + distance * 0.707)
            camera.SetViewUp(0, 0, 1)
        elif view == 'top':
            camera.SetPosition(cx, cy, cz + distance)
            camera.SetViewUp(0, 1, 0)
        elif view == 'bottom':
            camera.SetPosition(cx, cy, cz - distance)
            camera.SetViewUp(0, -1, 0)
        elif view == 'front':
            camera.SetPosition(cx, cy - distance, cz)
            camera.SetViewUp(0, 0, 1)
        elif view == 'back':
            camera.SetPosition(cx, cy + distance, cz)
            camera.SetViewUp(0, 0, 1)
        elif view == 'left':
            camera.SetPosition(cx - distance, cy, cz)
            camera.SetViewUp(0, 0, 1)
        elif view == 'right':
            camera.SetPosition(cx + distance, cy, cz)
            camera.SetViewUp(0, 0, 1)
            
        self.renderer.ResetCameraClippingRange()
        self.vtkWidget.GetRenderWindow().Render()
        
    def toggle_auto_rotate(self, enabled):
        """Start or stop the continuous camera orbit."""
        if enabled:
            if not self.rotate_timer.isActive():
                self.rotate_timer.start(16)  # ~60 FPS update loop
        else:
            self.rotate_timer.stop()
            
    def _auto_rotate_tick(self):
        """Rotate the active camera around the model Z axis on each timer tick."""
        if self.actor is None:
            return
            
        camera = self.renderer.GetActiveCamera()
        bounds = self.actor.GetBounds()
        
        cx = (bounds[0] + bounds[1]) / 2.0
        cy = (bounds[2] + bounds[3]) / 2.0
        
        # Get active coordinates
        px, py, pz = camera.GetPosition()
        
        # Calculate horizontal vector from focal point
        rx = px - cx
        ry = py - cy
        
        # Compute angle and orbit radius
        radius = math.sqrt(rx*rx + ry*ry)
        current_angle = math.atan2(ry, rx)
        
        # Increment angle
        new_angle = current_angle + math.radians(self.rotation_angle)
        
        # Re-position camera in XY plane
        new_px = cx + radius * math.cos(new_angle)
        new_py = cy + radius * math.sin(new_angle)
        
        camera.SetPosition(new_px, new_py, pz)
        self.vtkWidget.GetRenderWindow().Render()
