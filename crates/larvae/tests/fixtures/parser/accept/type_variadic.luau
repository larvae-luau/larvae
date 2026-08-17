--!strict
type Foo = (...number) -> ()
type Baz = (string, ...Foo) -> ...Foo
type Bar = (...number) -> (string, ...number) -> ...any
type Boom = (..."hit" | "miss") -> (string, ...("critical" | "weak" | "normal")) -> ...("dead" | "alive")

function _bar(...: number): ...number | string end

type Fn<U...> = any
type T = Fn<...'ok'>

return T
