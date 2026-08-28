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
-- parts a rig actually has and a hover names them. Every child is
-- optional: a part can be destroyed, detached, or not yet streamed in,
-- and a guaranteed type would hide the nil the runtime can hand back.

export type R15Character = Model & {
	HumanoidRootPart: Part?,
	Head: MeshPart?,

	UpperTorso: MeshPart?,
	LowerTorso: MeshPart?,

	LeftUpperArm: MeshPart?,
	LeftLowerArm: MeshPart?,
	LeftHand: MeshPart?,

	RightUpperArm: MeshPart?,
	RightLowerArm: MeshPart?,
	RightHand: MeshPart?,

	LeftUpperLeg: MeshPart?,
	LeftLowerLeg: MeshPart?,
	LeftFoot: MeshPart?,

	RightUpperLeg: MeshPart?,
	RightLowerLeg: MeshPart?,
	RightFoot: MeshPart?,

	Humanoid: (Humanoid & {
		Animator: Animator?,
		HumanoidDescription: HumanoidDescription?,
	})?,

	BodyColors: BodyColors?,

	-- Every spawned character carries these two, whatever the rig.
	Animate: LocalScript?,
	Health: Script?,
}

-- The R6 part names carry spaces, so the keys are quoted.
export type R6Character = Model & {
	HumanoidRootPart: Part?,
	Head: Part?,
	Torso: Part?,

	["Left Arm"]: Part?,
	["Right Arm"]: Part?,
	["Left Leg"]: Part?,
	["Right Leg"]: Part?,

	Humanoid: (Humanoid & {
		Animator: Animator?,
		HumanoidDescription: HumanoidDescription?,
	})?,

	BodyColors: BodyColors?,

	Animate: LocalScript?,
	Health: Script?,
}

--[[
The shape of `.config.luau`, Luau's config as code.

The session wraps the file's return in `_larvae_config_check`, so the
keys typecheck and the editor offers them. The names live in larvae's
hidden spelling, and no completion list shows them. luau-lsp wires the
same file the same way in its pull 1532.
]]
type _larvae_config_lints = {
	["*"]: boolean?,
	UnknownGlobal: boolean?,
	DeprecatedGlobal: boolean?,
	GlobalUsedAsLocal: boolean?,
	LocalShadow: boolean?,
	SameLineStatement: boolean?,
	MultiLineStatement: boolean?,
	LocalUnused: boolean?,
	FunctionUnused: boolean?,
	ImportUnused: boolean?,
	BuiltinGlobalWrite: boolean?,
	PlaceholderRead: boolean?,
	UnreachableCode: boolean?,
	UnknownType: boolean?,
	ForRange: boolean?,
	UnbalancedAssignment: boolean?,
	ImplicitReturn: boolean?,
	DuplicateLocal: boolean?,
	FormatString: boolean?,
	TableLiteral: boolean?,
	UninitializedLocal: boolean?,
	DuplicateFunction: boolean?,
	DeprecatedApi: boolean?,
	TableOperations: boolean?,
	DuplicateCondition: boolean?,
	MisleadingAndOr: boolean?,
	CommentDirective: boolean?,
	IntegerParsing: boolean?,
	ComparisonPrecedence: boolean?,
	RedundantNativeAttribute: boolean?,
}

type _larvae_config_file = {
	languagemode: ("strict" | "nonstrict" | "nocheck")?,
	lint: _larvae_config_lints?,
	linterrors: boolean?,
	typeerrors: boolean?,
	globals: { string }?,
	aliases: { [string]: string }?,
}

declare function _larvae_config_check(config: _larvae_config_file): _larvae_config_file
