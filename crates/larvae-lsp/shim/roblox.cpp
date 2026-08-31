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

    enumGlobalCarriesItsClass(globals);
}

/*
`Enum` answers to `Enums`, the class the API dump gives it.

The generated definitions type the global as a flat table of every
enum, because the table is what `Enum.KeyCode` indexes. The dump also
declares an `Enums` class with the methods the object really has, and
the two never met: a value annotated `Enums`, or a parameter that asks
for one, refused the global that is one.

The global takes the intersection of the two. Indexing still reads the
table, and the class half answers a name that asks for `Enums`, which
is what the runtime object is.
*/
void enumGlobalCarriesItsClass(Luau::GlobalTypes& globals)
{
    auto binding = globals.globalScope->bindings.find(
        globals.globalNames.names->getOrAdd("Enum")
    );

    if (binding == globals.globalScope->bindings.end())
        return;

    auto declared = globals.globalScope->exportedTypeBindings.find("Enums");

    if (declared == globals.globalScope->exportedTypeBindings.end())
        return;

    Luau::TypeId table = Luau::follow(binding->second.typeId);
    Luau::TypeId klass = Luau::follow(declared->second.type);

    if (table == klass)
        return;

    binding->second.typeId =
        globals.globalTypes.addType(Luau::IntersectionType{{table, klass}});
}
}
