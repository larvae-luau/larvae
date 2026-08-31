#pragma once

namespace Luau
{
struct GlobalTypes;
}

namespace Larvae
{
void registerRobloxEnums(Luau::GlobalTypes& globals);

/// Give the `Enum` global the `Enums` class beside its table of enums
void enumGlobalCarriesItsClass(Luau::GlobalTypes& globals);
}
