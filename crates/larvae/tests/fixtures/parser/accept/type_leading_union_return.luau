type UnionReturn = () -> | string | number
type IntersectionReturn = () -> & string & number
type NestedReturn = (string) -> | boolean | nil

local f: () -> | string | number = nil
local g: (number) -> & string & number = nil

return { f, g }
