--!strict
-- larvae's own definitions, layered over the Roblox globals.
--
-- The nightly that refreshes globalTypes.d.luau names that one file and
-- only that one, so nothing here is ever overwritten by it. Everything in
-- this file is larvae's to keep.
--
-- The character rigs. Roblox types Player.Character as Model?, which knows
-- no body parts, so every rig access needs a cast. `[lsp] character_type`
-- picks one of these and the analyzer puts it on the property.
--
-- Each child carries its real class, so a completion after a dot lists the
-- parts a rig actually has and a hover names them.

export type R15Character = Model & {
	HumanoidRootPart: Part,
	Head: MeshPart,

	UpperTorso: MeshPart,
	LowerTorso: MeshPart,

	LeftUpperArm: MeshPart,
	LeftLowerArm: MeshPart,
	LeftHand: MeshPart,

	RightUpperArm: MeshPart,
	RightLowerArm: MeshPart,
	RightHand: MeshPart,

	LeftUpperLeg: MeshPart,
	LeftLowerLeg: MeshPart,
	LeftFoot: MeshPart,

	RightUpperLeg: MeshPart,
	RightLowerLeg: MeshPart,
	RightFoot: MeshPart,

	Humanoid: Humanoid & {
		Animator: Animator,
		HumanoidDescription: HumanoidDescription,
	},

	BodyColors: BodyColors,

	-- Every spawned character carries these two, whatever the rig.
	Animate: LocalScript,
	Health: Script,
}

-- The R6 part names carry spaces, so the keys are quoted.
export type R6Character = Model & {
	HumanoidRootPart: Part,
	Head: Part,
	Torso: Part,

	["Left Arm"]: Part,
	["Right Arm"]: Part,
	["Left Leg"]: Part,
	["Right Leg"]: Part,

	Humanoid: Humanoid & {
		Animator: Animator,
		HumanoidDescription: HumanoidDescription,
	},

	BodyColors: BodyColors,

	Animate: LocalScript,
	Health: Script,
}
