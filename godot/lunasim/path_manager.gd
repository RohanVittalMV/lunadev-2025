extends Path3D

@export var test_path:Array[Vector3]
var path_mat : Material=preload("res://path_material.tres")
@onready var robot = $"../Robot"
@onready var line_mesh = $CSGPolygon3D


# Called when the node enters the scene tree for the first time.
func _ready() -> void:
	line_mesh.material=path_mat
	create_path(test_path)

func create_path(path:Array[Vector3]):
	self.curve.add_point(Vector3(robot.position.x, robot.position.y+0.5, robot.position.z))
	for i in path.size():
		var point_pos=Vector3(path[i].x, path[i].y, path[i].z)
		self.curve.add_point(point_pos)
		place_marker(point_pos,str(i))

#places cylinder maker for every point in path
# x and z parameters are for marker position
#num is for identifying which marker is being placed
func place_marker(pos:Vector3,num:String):
		var marker = CSGCylinder3D.new()
		marker.position=Vector3(pos.x,(pos.y/2)+0.05,pos.z)
		marker.height=pos.y
		marker.radius=0.05
		marker.name = str("marker",num)
		marker.material=path_mat
		add_child(marker)
