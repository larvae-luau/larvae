#pragma once

#include <string>
#include <vector>

namespace Luau
{
struct GlobalTypes;
struct ExternType;
}

namespace Larvae
{
void registerRobloxEnums(Luau::GlobalTypes& globals);

/// Give the `Enum` global the `Enums` class beside its table of enums
void enumGlobalCarriesItsClass(Luau::GlobalTypes& globals);

/// The two lists the `--#METADATA#` line of the Roblox definitions carries
struct RobloxMetadata
{
    std::vector<std::string> services;
    std::vector<std::string> creatable;
};

/// Read the metadata line off the top of one definitions text
RobloxMetadata parseRobloxMetadata(const std::string& source);

/// Put the handlers luau-lsp puts on the Roblox classes. See roblox.cpp.
void attachRobloxMagic(Luau::GlobalTypes& globals);

/// Make `Vector3` and `vector` one type, the way the runtime has them
void unifyVector3(Luau::GlobalTypes& globals);

/// Whether a class is the sourcemap's own spelling of one instance
bool isGeneratedInstance(const Luau::ExternType* type);

/// Every class under `Instance`, for a string completion
std::vector<std::string> instanceClassNames(const Luau::GlobalTypes& globals);

/// Every enum, for a string completion
std::vector<std::string> enumNames(const Luau::GlobalTypes& globals);

/// The properties of one class and its parents, without functions and events
std::vector<std::string> propertyNames(const Luau::ExternType* type);

/// The sourcemap children of one generated class
std::vector<std::string> childNames(const Luau::ExternType* type);
}
