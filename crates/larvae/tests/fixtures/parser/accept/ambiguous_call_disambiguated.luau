-- a `(` on the line of the callee is a call, not an ambiguity
local x = f(
	1,
	2
)
local y = obj:method(1):chain(2).field
print(1);
(f)()
return x, y
