/*
The Roblox definitions declare each enum as a flat exported type, ex:
`EnumKeyCode`, because a definition file cannot declare a type namespace.
Source code names that type `Enum.KeyCode`. Move the public enum types into
Luau's imported `Enum` namespace after the definitions load, and keep the
backing types available only through the values that use them.
*/

#include "roblox.h"

#include "Luau/Common.h"
#include "Luau/Frontend.h"
#include "Luau/Type.h"
#include "Luau/TypeAttach.h"

#include <optional>
#include <string>
#include <unordered_map>

namespace Larvae
{
void registerRobloxEnums(Luau::GlobalTypes& globals)
{
    std::unordered_map<Luau::Name, Luau::TypeFun> enumTypes;

    for (auto it = globals.globalScope->exportedTypeBindings.begin();
         it != globals.globalScope->exportedTypeBindings.end();)
    {
        bool erase = false;
        Luau::TypeId type = it->second.type;
        auto* external = Luau::getMutable<Luau::ExternType>(type);

        if (external && Luau::startsWith(external->name, "Enum"))
        {
            if (external->name != "Enum" && external->name != "Enums" && external->name != "EnumItem")
            {
                external->name.erase(0, 4);

                const size_t internal = external->name.rfind("_INTERNAL");
                if (internal != std::string::npos && internal + 9 == external->name.size())
                    external->name.erase(internal);
                else
                    enumTypes.emplace(external->name, it->second);

                Luau::asMutable(type)->documentationSymbol = "@roblox/enum/" + external->name;

                for (auto& [name, property] : external->props)
                {
                    property.documentationSymbol = "@roblox/enum/" + external->name + "." + name;
                    Luau::attachTag(property, "EnumItem");
                }

                external->name = "Enum." + external->name;
                erase = true;
            }

            external->metatable = std::nullopt;
        }

        if (erase)
            it = globals.globalScope->exportedTypeBindings.erase(it);
        else
            ++it;
    }

    globals.globalScope->importedTypeBindings.emplace("Enum", std::move(enumTypes));
}
}
