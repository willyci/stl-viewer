import os
import struct
import zipfile
import xml.etree.ElementTree as ET
import numpy as np

def strip_namespace(tag):
    """Strip XML namespace prefix from tag names."""
    if tag.startswith('{'):
        return tag.split('}', 1)[1]
    return tag

def get_attrib_insensitive(elem, name, default=None):
    """Retrieve an attribute value case-insensitively and ignoring XML namespace prefix in the attribute name."""
    name_lower = name.lower()
    for key, value in elem.attrib.items():
        key_local = strip_namespace(key).lower()
        if key_local == name_lower:
            return value
    return default

class ModelData:
    """Standardized 3D Model Geometry Container."""
    def __init__(self, vertices, triangles, file_format="", filename=""):
        self.vertices = vertices  # numpy array of shape (N, 3), float32
        self.triangles = triangles  # numpy array of shape (M, 3), int32
        self.format = file_format
        self.filename = filename
        
        # Metadata
        self.num_vertices = len(vertices)
        self.num_triangles = len(triangles)
        self.bounds = self._calculate_bounds()
        self.volume = self._calculate_volume()
        
    def _calculate_bounds(self):
        """Calculate bounding box dimensions [[xmin, xmax], [ymin, ymax], [zmin, zmax]]."""
        if self.num_vertices == 0:
            return [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]
        return [
            [float(np.min(self.vertices[:, 0])), float(np.max(self.vertices[:, 0]))],
            [float(np.min(self.vertices[:, 1])), float(np.max(self.vertices[:, 1]))],
            [float(np.min(self.vertices[:, 2])), float(np.max(self.vertices[:, 2]))]
        ]
        
    def _calculate_volume(self):
        """Calculate signed volume of the closed mesh in cubic millimeters (or cubic units)."""
        if self.num_vertices == 0 or self.num_triangles == 0:
            return 0.0
        # Tetrahedron volume from origin formula: 1/6 * dot(A, cross(B, C))
        A = self.vertices[self.triangles[:, 0]]
        B = self.vertices[self.triangles[:, 1]]
        C = self.vertices[self.triangles[:, 2]]
        
        # Vectorized triple scalar product
        vol = np.sum(A * np.cross(B, C), axis=1) / 6.0
        return float(np.abs(np.sum(vol)))

    @property
    def bounding_box_size(self):
        """Return the size in X, Y, Z directions."""
        return [
            self.bounds[0][1] - self.bounds[0][0],
            self.bounds[1][1] - self.bounds[1][0],
            self.bounds[2][1] - self.bounds[2][0]
        ]


def parse_stl(filepath):
    """Parse STL file (automatically determines if it is ASCII or Binary)."""
    # Detect format
    is_ascii = True
    with open(filepath, 'rb') as f:
        # Check first 5 bytes for 'solid'
        prefix = f.read(5)
        if len(prefix) == 5 and prefix == b'solid':
            # Check a bit further to verify it's not a binary file starting with 'solid'
            # (which is a common issue with some exporters)
            sample = f.read(1024)
            # Binary files typically contain non-printable ascii characters
            # or lack newlines and typical keywords like 'facet normal'
            if b'facet' not in sample and b'outer' not in sample:
                is_ascii = False
        else:
            is_ascii = False
            
    if is_ascii:
        return _parse_stl_ascii(filepath)
    else:
        return _parse_stl_binary(filepath)


def _parse_stl_ascii(filepath):
    """Load ASCII STL file."""
    vertices = []
    
    with open(filepath, 'r', errors='ignore') as f:
        for line in f:
            line_strip = line.strip().lower()
            if line_strip.startswith('vertex'):
                parts = line_strip.split()
                if len(parts) >= 4:
                    try:
                        vertices.append([float(parts[1]), float(parts[2]), float(parts[3])])
                    except ValueError:
                        pass
                        
    if len(vertices) == 0:
        raise ValueError("No vertices found in ASCII STL file.")
        
    flat_vertices = np.array(vertices, dtype=np.float32)
    # Deduplicate vertices
    unique_vertices, inverse_indices = np.unique(flat_vertices, axis=0, return_inverse=True)
    triangles = inverse_indices.reshape(-1, 3)
    
    return ModelData(unique_vertices, triangles, file_format="STL (ASCII)", filename=os.path.basename(filepath))


def _parse_stl_binary(filepath):
    """Load Binary STL file using fast Numpy block operations."""
    with open(filepath, 'rb') as f:
        header = f.read(80)
        count_bytes = f.read(4)
        if len(count_bytes) < 4:
            raise ValueError("Corrupt Binary STL file: Cannot read triangle count.")
            
        num_triangles = struct.unpack('<I', count_bytes)[0]
        data = f.read()
        
    # Check if actual bytes match the stated triangle count
    # Each triangle records 50 bytes: 3*4=12 floats for normals/vertices (48 bytes) + 2 bytes attribute
    expected_size = num_triangles * 50
    if len(data) < expected_size:
        # File is truncated or triangle count is mislabeled, adjust to fit whatever data is present
        num_triangles = len(data) // 50
        
    # Read vertices using custom numpy dtype
    dtype = np.dtype([
        ('normal', '<f4', (3,)),
        ('vertices', '<f4', (3, 3)),
        ('attr', '<u2')
    ])
    
    records = np.frombuffer(data[:num_triangles * 50], dtype=dtype)
    flat_vertices = records['vertices'].reshape(-1, 3)
    
    # Deduplicate vertices for smooth shading index resolution
    unique_vertices, inverse_indices = np.unique(flat_vertices, axis=0, return_inverse=True)
    triangles = inverse_indices.reshape(-1, 3)
    
    return ModelData(unique_vertices, triangles, file_format="STL (Binary)", filename=os.path.basename(filepath))


def parse_3mf_transform(transform_str):
    """Parse 3MF 4x3 row-major transformation matrix into rotation matrix and translation vector."""
    if not transform_str:
        return np.eye(3, dtype=np.float32), np.zeros(3, dtype=np.float32)
        
    try:
        m = list(map(float, transform_str.split()))
        if len(m) != 12:
            return np.eye(3, dtype=np.float32), np.zeros(3, dtype=np.float32)
            
        # Row 1: m[0], m[1], m[2]
        # Row 2: m[3], m[4], m[5]
        # Row 3: m[6], m[7], m[8]
        # Translation: m[9], m[10], m[11]
        R = np.array([
            [m[0], m[1], m[2]],
            [m[3], m[4], m[5]],
            [m[6], m[7], m[8]]
        ], dtype=np.float32)
        T = np.array([m[9], m[10], m[11]], dtype=np.float32)
        return R, T
    except Exception:
        return np.eye(3, dtype=np.float32), np.zeros(3, dtype=np.float32)


def _apply_mesh_transform(vertices, R, T):
    """Apply R and T matrix transform to vertices."""
    if len(vertices) == 0:
        return vertices
    return vertices.dot(R.T) + T


def parse_3mf(filepath):
    """Load a 3MF (.3mf) zip file, resolve its internal structure and merge all instances."""
    if not zipfile.is_zipfile(filepath):
        raise ValueError("File is not a valid 3MF zip archive.")
        
    with zipfile.ZipFile(filepath, 'r') as z:
        zip_file_names = z.namelist()
        
        # Helper to find a file inside the zip case-insensitively and normalize it
        def find_zip_entry(path_str):
            norm_path = path_str.replace('\\', '/').lstrip('/')
            if norm_path in zip_file_names:
                return norm_path
            norm_path_lower = norm_path.lower()
            for name in zip_file_names:
                if name.lower() == norm_path_lower:
                    return name
            return None
            
        # Check standard 3dmodel path
        root_model_path = '3D/3dmodel.model'
        root_model_path_norm = find_zip_entry(root_model_path)
        if not root_model_path_norm:
            raise ValueError("Could not locate 3D/3dmodel.model in 3MF archive.")
            
        # Dictionary to store all parsed objects across all model files:
        # Key: (model_path_normalized, obj_id)
        # Value: (vertices, triangles, components_list)
        # where components_list contains: (comp_objectid, comp_path, transform_matrix_R_T)
        objects = {}
        
        # Track which model files we have already parsed to avoid duplicate parsing
        parsed_model_files = set()
        
        def parse_model_file(model_path):
            norm_path = find_zip_entry(model_path)
            if not norm_path or norm_path in parsed_model_files:
                return
                
            parsed_model_files.add(norm_path)
            
            try:
                xml_content = z.read(norm_path)
                root_el = ET.fromstring(xml_content)
            except Exception:
                return
                
            resources_node = None
            for child in root_el:
                if strip_namespace(child.tag) == 'resources':
                    resources_node = child
                    break
                    
            if resources_node is None:
                return
                
            for obj in resources_node:
                if strip_namespace(obj.tag) != 'object':
                    continue
                    
                obj_id = get_attrib_insensitive(obj, 'id')
                if not obj_id:
                    continue
                    
                obj_vertices = []
                obj_triangles = []
                obj_components = []
                
                for details in obj:
                    tag = strip_namespace(details.tag)
                    if tag == 'mesh':
                        vertex_elem = None
                        triangle_elem = None
                        for mesh_child in details:
                            mesh_tag = strip_namespace(mesh_child.tag)
                            if mesh_tag == 'vertices':
                                vertex_elem = mesh_child
                            elif mesh_tag == 'triangles':
                                triangle_elem = mesh_child
                                
                        if vertex_elem is not None:
                            for v in vertex_elem:
                                if strip_namespace(v.tag) == 'vertex':
                                    obj_vertices.append([
                                        float(get_attrib_insensitive(v, 'x', 0)),
                                        float(get_attrib_insensitive(v, 'y', 0)),
                                        float(get_attrib_insensitive(v, 'z', 0))
                                    ])
                                    
                        if triangle_elem is not None:
                            for t in triangle_elem:
                                if strip_namespace(t.tag) == 'triangle':
                                    obj_triangles.append([
                                        int(get_attrib_insensitive(t, 'v1', 0)),
                                        int(get_attrib_insensitive(t, 'v2', 0)),
                                        int(get_attrib_insensitive(t, 'v3', 0))
                                    ])
                    elif tag == 'components':
                        for comp in details:
                            if strip_namespace(comp.tag) == 'component':
                                comp_objectid = get_attrib_insensitive(comp, 'objectid')
                                comp_transform = get_attrib_insensitive(comp, 'transform')
                                comp_path = get_attrib_insensitive(comp, 'path')
                                
                                R, T = parse_3mf_transform(comp_transform)
                                obj_components.append((comp_objectid, comp_path, (R, T)))
                                
                objects[(norm_path, obj_id)] = (
                    np.array(obj_vertices, dtype=np.float32) if obj_vertices else np.zeros((0, 3), dtype=np.float32),
                    np.array(obj_triangles, dtype=np.int32) if obj_triangles else np.zeros((0, 3), dtype=np.int32),
                    obj_components
                )

        # Helper to recursively construct mesh for a given model path and object ID
        def resolve_object(current_model_path, obj_id, visited=None):
            if visited is None:
                visited = set()
                
            norm_model_path = find_zip_entry(current_model_path)
            if not norm_model_path:
                return np.zeros((0, 3), dtype=np.float32), np.zeros((0, 3), dtype=np.int32)
                
            if norm_model_path not in parsed_model_files:
                parse_model_file(norm_model_path)
                
            object_key = (norm_model_path, obj_id)
            if object_key in visited:
                return np.zeros((0, 3), dtype=np.float32), np.zeros((0, 3), dtype=np.int32)
                
            if object_key not in objects:
                return np.zeros((0, 3), dtype=np.float32), np.zeros((0, 3), dtype=np.int32)
                
            visited.add(object_key)
            mesh_verts, mesh_tris, comps = objects[object_key]
            
            all_vertices = [mesh_verts] if len(mesh_verts) > 0 else []
            all_triangles = [mesh_tris] if len(mesh_tris) > 0 else []
            
            current_vert_offset = len(mesh_verts)
            
            for comp_id, comp_path, (R, T) in comps:
                target_model_path = comp_path if comp_path else norm_model_path
                
                c_verts, c_tris = resolve_object(target_model_path, comp_id, visited.copy())
                if len(c_verts) == 0:
                    continue
                    
                c_verts_trans = _apply_mesh_transform(c_verts, R, T)
                
                all_vertices.append(c_verts_trans)
                all_triangles.append(c_tris + current_vert_offset)
                
                current_vert_offset += len(c_verts)
                
            if not all_vertices:
                return np.zeros((0, 3), dtype=np.float32), np.zeros((0, 3), dtype=np.int32)
                
            final_verts = np.vstack(all_vertices)
            final_tris = np.vstack(all_triangles) if all_triangles else np.zeros((0, 3), dtype=np.int32)
            
            return final_verts, final_tris

        # Trigger initial parse of the root model
        parse_model_file(root_model_path_norm)
        
        # Load build node from root model
        try:
            xml_content = z.read(root_model_path_norm)
            root_el = ET.fromstring(xml_content)
        except Exception as e:
            raise ValueError(f"Failed to read root XML inside 3MF model: {e}")
            
        build_node = None
        for child in root_el:
            if strip_namespace(child.tag) == 'build':
                build_node = child
                break
                
        # Resolve items in <build> block
        final_build_vertices = []
        final_build_triangles = []
        vert_offset = 0
        
        if build_node is not None:
            for item in build_node:
                if strip_namespace(item.tag) != 'item':
                    continue
                    
                obj_id = get_attrib_insensitive(item, 'objectid')
                if not obj_id:
                    continue
                    
                item_transform = get_attrib_insensitive(item, 'transform')
                R, T = parse_3mf_transform(item_transform)
                
                item_verts, item_tris = resolve_object(root_model_path_norm, obj_id)
                if len(item_verts) == 0:
                    continue
                    
                item_verts_trans = _apply_mesh_transform(item_verts, R, T)
                
                final_build_vertices.append(item_verts_trans)
                final_build_triangles.append(item_tris + vert_offset)
                
                vert_offset += len(item_verts)
                
        # If build is empty, try to display all valid mesh objects in the resource block
        if not final_build_vertices and objects:
            referenced_keys = set()
            for (model_path, obj_id), (_, _, comps) in objects.items():
                for comp_id, comp_path, _ in comps:
                    target_path = comp_path if comp_path else model_path
                    target_path_norm = find_zip_entry(target_path)
                    if target_path_norm:
                        referenced_keys.add((target_path_norm, comp_id))
                        
            # Resolve all top-level objects
            for (model_path, obj_id) in objects:
                if (model_path, obj_id) not in referenced_keys:
                    item_verts, item_tris = resolve_object(model_path, obj_id)
                    if len(item_verts) > 0:
                        final_build_vertices.append(item_verts)
                        final_build_triangles.append(item_tris + vert_offset)
                        vert_offset += len(item_verts)
                        
            # If still empty (e.g. cyclic component structure or no distinct top-level),
            # resolve everything that has geometry
            if not final_build_vertices:
                for (model_path, obj_id) in objects:
                    item_verts, item_tris = resolve_object(model_path, obj_id)
                    if len(item_verts) > 0:
                        final_build_vertices.append(item_verts)
                        final_build_triangles.append(item_tris + vert_offset)
                        vert_offset += len(item_verts)
                        
        if not final_build_vertices:
            raise ValueError("No displayable meshes or build items found in 3MF file.")
            
        combined_vertices = np.vstack(final_build_vertices)
        combined_triangles = np.vstack(final_build_triangles)
        
        # Deduplicate vertices and map the old triangle references to the unique vertices indices
        unique_vertices, inverse_indices = np.unique(combined_vertices, axis=0, return_inverse=True)
        triangles = inverse_indices[combined_triangles]
        
        return ModelData(unique_vertices, triangles, file_format="3MF Model", filename=os.path.basename(filepath))


def parse_obj(filepath):
    """Parse Wavefront OBJ (.obj) mesh file rapidly."""
    vertices = []
    triangles = []
    
    with open(filepath, 'r', errors='ignore') as f:
        for line in f:
            if line.startswith('v '):
                parts = line.split()
                if len(parts) >= 4:
                    try:
                        vertices.append([float(parts[1]), float(parts[2]), float(parts[3])])
                    except ValueError:
                        pass
            elif line.startswith('f '):
                parts = line.split()
                if len(parts) >= 4:
                    face_indices = []
                    for p in parts[1:]:
                        try:
                            # OBJ indices are 1-based, can contain vt/vn (e.g., v/vt/vn)
                            idx = int(p.split('/')[0])
                            # Handle relative negative indices
                            if idx < 0:
                                idx = len(vertices) + idx
                            else:
                                idx = idx - 1
                            face_indices.append(idx)
                        except ValueError:
                            pass
                            
                    # Triangulate polygon fans for meshes that have quads/n-gons
                    for i in range(1, len(face_indices) - 1):
                        triangles.append([face_indices[0], face_indices[i], face_indices[i+1]])
                        
    if len(vertices) == 0:
        raise ValueError("No valid geometry found in Wavefront OBJ file.")
        
    flat_vertices = np.array(vertices, dtype=np.float32)
    # Deduplicate vertices for smooth shading
    unique_vertices, inverse_indices = np.unique(flat_vertices, axis=0, return_inverse=True)
    triangles_array = np.array(triangles, dtype=np.int32)
    
    # Map old indices to unique indices
    if len(triangles_array) > 0:
        mapped_triangles = inverse_indices[triangles_array]
    else:
        mapped_triangles = np.zeros((0, 3), dtype=np.int32)
        
    return ModelData(unique_vertices, mapped_triangles, file_format="Wavefront OBJ", filename=os.path.basename(filepath))


def parse_gcode(filepath):
    """Parse a GCODE (.gcode) file and convert print movements into 3D line paths."""
    vertices = []
    lines = []
    
    # State tracking
    curr_x, curr_y, curr_z = 0.0, 0.0, 0.0
    # Keep track of absolute/relative positioning
    is_absolute = True
    
    with open(filepath, 'r', errors='ignore') as f:
        for line in f:
            # Strip comments
            line_clean = line.split(';')[0].strip()
            if not line_clean:
                continue
                
            parts = line_clean.split()
            cmd = parts[0].upper() if parts else ""
            
            if cmd == 'G90':
                is_absolute = True
            elif cmd == 'G91':
                is_absolute = False
            elif cmd in ('G0', 'G1'):
                # G0/G1 movements
                params = {}
                for p in parts[1:]:
                    if len(p) > 1:
                        char = p[0].upper()
                        try:
                            params[char] = float(p[1:])
                        except ValueError:
                            pass
                            
                new_x = curr_x
                new_y = curr_y
                new_z = curr_z
                
                if 'X' in params:
                    new_x = params['X'] if is_absolute else curr_x + params['X']
                if 'Y' in params:
                    new_y = params['Y'] if is_absolute else curr_y + params['Y']
                if 'Z' in params:
                    new_z = params['Z'] if is_absolute else curr_z + params['Z']
                    
                is_extruding = ('E' in params and params['E'] > 0) or cmd == 'G1'
                
                # Update position
                if (new_x != curr_x or new_y != curr_y or new_z != curr_z):
                    if is_extruding:
                        # Add vertices and define line segment
                        p1_idx = len(vertices)
                        vertices.append([curr_x, curr_y, curr_z])
                        vertices.append([new_x, new_y, new_z])
                        lines.append([p1_idx, p1_idx + 1])
                        
                    curr_x, curr_y, curr_z = new_x, new_y, new_z
                    
    if len(vertices) == 0:
        raise ValueError("No valid print movement paths found in GCODE file.")
        
    flat_vertices = np.array(vertices, dtype=np.float32)
    flat_lines = np.array(lines, dtype=np.int32)
    
    # Standardize ModelData
    model = ModelData(flat_vertices, np.zeros((0, 3), dtype=np.int32), file_format="G-Code Path", filename=os.path.basename(filepath))
    model.lines = flat_lines
    model.num_triangles = 0
    model.num_lines = len(flat_lines)
    return model


def load_model(filepath):
    """High-level Loader supporting STL, 3MF, OBJ, and GCODE files based on extension."""
    ext = os.path.splitext(filepath)[1].lower()
    if ext == '.stl':
        return parse_stl(filepath)
    elif ext == '.3mf':
        return parse_3mf(filepath)
    elif ext == '.obj':
        return parse_obj(filepath)
    elif ext in ('.gcode', '.gco'):
        return parse_gcode(filepath)
    else:
        raise ValueError(f"Unsupported file format: {ext}")
