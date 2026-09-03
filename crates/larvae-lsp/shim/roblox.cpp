/*
What the Roblox definitions cannot say on their own.

A declaration file names every class and every member, and stops there.
It cannot say that `IsA` narrows its receiver, that `Clone` answers with
the class it was called on, or that `Vector3` is the `vector` the runtime
hands out. luau-lsp closes those holes with magic handlers after the file
loads, and this file does the same, handler for handler, so a project
reads the same in both.
*/

#include "roblox.h"

#include "Luau/Ast.h"
#include "Luau/BuiltinDefinitions.h"
#include "Luau/Common.h"
#include "Luau/ConstraintSolver.h"
#include "Luau/Error.h"
#include "Luau/Frontend.h"
#include "Luau/LValue.h"
#include "Luau/Predicate.h"
#include "Luau/Scope.h"
#include "Luau/Type.h"
#include "Luau/TypeAttach.h"
#include "Luau/TypeInfer.h"
#include "Luau/TypePack.h"

#include <algorithm>
#include <memory>
#include <optional>
#include <set>
#include <string>
#include <unordered_map>
#include <vector>

LUAU_FASTFLAG(LuauCyclicRequireTypeInference)
LUAU_FASTFLAG(LuauSolverV2)

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

// --- the metadata line ------------------------------------------------------

/*
The strings of one JSON array, read off the metadata line.

The line is `--#METADATA#{"CREATABLE_INSTANCES": [...], "SERVICES": [...]}`,
two flat arrays of names and nothing else, so a full JSON reader would be
weight for no gain. The reader finds the key, then takes every quoted
string until the bracket closes.
*/
static std::vector<std::string> arrayUnder(const std::string& line, const char* key)
{
    std::vector<std::string> out;

    size_t at = line.find(std::string("\"") + key + "\"");
    if (at == std::string::npos)
        return out;

    at = line.find('[', at);
    if (at == std::string::npos)
        return out;

    while (at < line.size() && line[at] != ']')
    {
        if (line[at] == '"')
        {
            size_t end = line.find('"', at + 1);
            if (end == std::string::npos)
                break;

            out.push_back(line.substr(at + 1, end - at - 1));
            at = end + 1;
            continue;
        }

        ++at;
    }

    return out;
}

RobloxMetadata parseRobloxMetadata(const std::string& source)
{
    RobloxMetadata out;

    const std::string marker = "--#METADATA#";
    size_t at = source.find(marker);
    if (at == std::string::npos)
        return out;

    size_t end = source.find('\n', at);
    std::string line = source.substr(at, end == std::string::npos ? std::string::npos : end - at);

    out.services = arrayUnder(line, "SERVICES");
    out.creatable = arrayUnder(line, "CREATABLE_INSTANCES");

    return out;
}

// --- shared pieces of the handlers ------------------------------------------

namespace
{
/// The first argument of a call, when the author wrote it as a string
std::optional<std::string> firstString(const Luau::AstExprCall& call)
{
    if (call.args.size < 1)
        return std::nullopt;

    auto* text = call.args.data[0]->as<Luau::AstExprConstantString>();
    if (!text)
        return std::nullopt;

    return std::string(text->value.data, text->value.size);
}

/// The class a name stands for, when it is a plain one
std::optional<Luau::TypeId> classNamed(const Luau::Scope* scope, const std::string& name)
{
    std::optional<Luau::TypeFun> found = scope->lookupType(name);

    if (!found || !found->typeParams.empty() || !found->typePackParams.empty())
        return std::nullopt;

    return Luau::follow(found->type);
}

/// Report to the new solver, whichever way this Luau spells it
void complain(const Luau::MagicFunctionCallContext& context, Luau::TypeErrorData data, const Luau::Location& where)
{
    if (FFlag::LuauCyclicRequireTypeInference)
        context.solver->reportError(std::move(data), where, *context.constraint->moduleName);
    else
        context.solver->DEPRECATED_reportError(std::move(data), where);
}

/// Answer the new solver with one type
void answer(const Luau::MagicFunctionCallContext& context, Luau::TypeId type)
{
    Luau::asMutable(context.result)
        ->ty.emplace<Luau::BoundTypePack>(context.solver->arena->addTypePack({type}));
}

/// Answer the old solver with one type
Luau::WithPredicate<Luau::TypePackId> answer(Luau::TypeChecker& typeChecker, Luau::TypeId type)
{
    Luau::TypeArena& arena = *typeChecker.currentModule->internalTypes;

    return Luau::WithPredicate<Luau::TypePackId>{arena.addTypePack({type})};
}

/// The receiver of a method call, as the old solver sees it
std::optional<Luau::TypeId> receiver(
    Luau::TypeChecker& typeChecker, const Luau::ScopePtr& scope, const Luau::AstExprCall& call)
{
    auto* index = call.func->as<Luau::AstExprIndexName>();
    if (!index)
        return std::nullopt;

    return typeChecker.checkExpr(scope, *index->expr).type;
}

// --- `instance:IsA("Part")` --------------------------------------------------

/*
`IsA` narrows its receiver to the class it names.

The declaration says `IsA` answers a boolean, which is true, and the
whole point of the call is what a reader does with that boolean: in the
branch where it held, the instance has every member of the class. The
old solver takes that as a predicate on the receiver, and the new one
binds the discriminant the refinement asks about.
*/
struct MagicInstanceIsA final : Luau::MagicFunction
{
    std::optional<Luau::WithPredicate<Luau::TypePackId>> handleOldSolver(
        Luau::TypeChecker& typeChecker,
        const Luau::ScopePtr&,
        const Luau::AstExprCall& call,
        Luau::WithPredicate<Luau::TypePackId>) override
    {
        if (call.args.size != 1)
            return std::nullopt;

        auto* index = call.func->as<Luau::AstExprIndexName>();
        std::optional<std::string> name = firstString(call);
        if (!index || !name)
            return std::nullopt;

        std::optional<Luau::LValue> lvalue = Luau::tryGetLValue(*index->expr);
        if (!lvalue)
            return std::nullopt;

        std::optional<Luau::TypeId> type = classNamed(typeChecker.globalScope.get(), *name);
        if (!type)
        {
            typeChecker.reportError(Luau::TypeError{
                call.args.data[0]->location, Luau::UnknownSymbol{*name, Luau::UnknownSymbol::Type}});
            return std::nullopt;
        }

        Luau::TypeArena& arena = *typeChecker.currentModule->internalTypes;
        Luau::TypePackId booleans = arena.addTypePack({typeChecker.booleanType});

        return Luau::WithPredicate<Luau::TypePackId>{
            booleans, {Luau::IsAPredicate{std::move(*lvalue), call.location, *type}}};
    }

    bool infer(const Luau::MagicFunctionCallContext& context) override
    {
        if (context.callSite->args.size != 1)
            return false;

        auto* index = context.callSite->func->as<Luau::AstExprIndexName>();
        std::optional<std::string> name = firstString(*context.callSite);
        if (!index || !name)
            return false;

        if (!context.solver->rootScope->lookupType(*name))
            complain(context, Luau::UnknownSymbol{*name, Luau::UnknownSymbol::Type},
                context.callSite->args.data[0]->location);

        return false;
    }

    void refine(const Luau::MagicRefinementContext& context) override
    {
        if (context.callSite->args.size != 1 || context.discriminantTypes.empty())
            return;

        auto* index = context.callSite->func->as<Luau::AstExprIndexName>();
        std::optional<std::string> name = firstString(*context.callSite);
        if (!index || !name)
            return;

        std::optional<Luau::TypeId> discriminant = context.discriminantTypes[0];
        if (!discriminant || !Luau::get<Luau::BlockedType>(*discriminant))
            return;

        std::optional<Luau::TypeFun> found = context.scope->lookupType(*name);
        if (!found)
            return;

        Luau::asMutable(*discriminant)->ty.emplace<Luau::BoundType>(Luau::follow(found->type));
    }
};

// --- `item:IsA("KeyCode")` -------------------------------------------------

/// `IsA` on an enum item narrows to the enum, which lives under `Enum.`
struct MagicEnumItemIsA final : Luau::MagicFunction
{
    std::optional<Luau::WithPredicate<Luau::TypePackId>> handleOldSolver(
        Luau::TypeChecker& typeChecker,
        const Luau::ScopePtr& scope,
        const Luau::AstExprCall& call,
        Luau::WithPredicate<Luau::TypePackId>) override
    {
        if (call.args.size != 1)
            return std::nullopt;

        auto* index = call.func->as<Luau::AstExprIndexName>();
        std::optional<std::string> name = firstString(call);
        if (!index || !name)
            return std::nullopt;

        std::optional<Luau::LValue> lvalue = Luau::tryGetLValue(*index->expr);
        if (!lvalue)
            return std::nullopt;

        std::optional<Luau::TypeFun> found = scope->lookupImportedType("Enum", *name);
        if (!found || !found->typeParams.empty() || !found->typePackParams.empty())
        {
            typeChecker.reportError(Luau::TypeError{
                call.args.data[0]->location, Luau::UnknownSymbol{*name, Luau::UnknownSymbol::Type}});
            return std::nullopt;
        }

        Luau::TypeArena& arena = *typeChecker.currentModule->internalTypes;
        Luau::TypePackId booleans = arena.addTypePack({typeChecker.booleanType});

        return Luau::WithPredicate<Luau::TypePackId>{
            booleans, {Luau::IsAPredicate{std::move(*lvalue), call.location, Luau::follow(found->type)}}};
    }

    bool infer(const Luau::MagicFunctionCallContext& context) override
    {
        if (context.callSite->args.size != 1)
            return false;

        auto* index = context.callSite->func->as<Luau::AstExprIndexName>();
        std::optional<std::string> name = firstString(*context.callSite);
        if (!index || !name)
            return false;

        if (!context.constraint->scope->lookupImportedType("Enum", *name))
            complain(context, Luau::UnknownSymbol{*name, Luau::UnknownSymbol::Type},
                context.callSite->args.data[0]->location);

        return false;
    }

    void refine(const Luau::MagicRefinementContext& context) override
    {
        if (context.callSite->args.size != 1 || context.discriminantTypes.empty())
            return;

        auto* index = context.callSite->func->as<Luau::AstExprIndexName>();
        std::optional<std::string> name = firstString(*context.callSite);
        if (!index || !name)
            return;

        std::optional<Luau::TypeId> discriminant = context.discriminantTypes[0];
        if (!discriminant || !Luau::get<Luau::BlockedType>(*discriminant))
            return;

        std::optional<Luau::TypeFun> found = context.scope->lookupImportedType("Enum", *name);
        if (!found)
            return;

        Luau::asMutable(*discriminant)->ty.emplace<Luau::BoundType>(Luau::follow(found->type));
    }
};

// --- `instance:Clone()` and `Instance.fromExisting(instance)` --------------

/// `Clone` answers with the class it was called on, not with `Instance`
struct MagicInstanceClone final : Luau::MagicFunction
{
    std::optional<Luau::WithPredicate<Luau::TypePackId>> handleOldSolver(
        Luau::TypeChecker& typeChecker,
        const Luau::ScopePtr& scope,
        const Luau::AstExprCall& call,
        Luau::WithPredicate<Luau::TypePackId>) override
    {
        std::optional<Luau::TypeId> self = receiver(typeChecker, scope, call);
        if (!self)
            return std::nullopt;

        return answer(typeChecker, *self);
    }

    bool infer(const Luau::MagicFunctionCallContext& context) override
    {
        if (!context.callSite->func->as<Luau::AstExprIndexName>())
            return false;

        std::optional<Luau::TypeId> self = Luau::first(context.arguments);
        if (!self)
            return false;

        answer(context, *self);
        return true;
    }
};

/// `fromExisting` answers with the class of the instance it copies
struct MagicInstanceFromExisting final : Luau::MagicFunction
{
    std::optional<Luau::WithPredicate<Luau::TypePackId>> handleOldSolver(
        Luau::TypeChecker& typeChecker,
        const Luau::ScopePtr& scope,
        const Luau::AstExprCall& call,
        Luau::WithPredicate<Luau::TypePackId>) override
    {
        if (call.args.size < 1)
            return std::nullopt;

        return answer(typeChecker, typeChecker.checkExpr(scope, *call.args.data[0]).type);
    }

    bool infer(const Luau::MagicFunctionCallContext& context) override
    {
        if (context.callSite->args.size < 1)
            return false;

        std::optional<Luau::TypeId> copied = Luau::first(context.arguments);
        if (!copied)
            return false;

        answer(context, *copied);
        return true;
    }
};

// --- `instance:FindFirstChildWhichIsA("Part")` and its three siblings --------

/// A lookup by class answers with that class, or nil
struct MagicFindFirstWhichIsA final : Luau::MagicFunction
{
    std::optional<Luau::WithPredicate<Luau::TypePackId>> handleOldSolver(
        Luau::TypeChecker& typeChecker,
        const Luau::ScopePtr&,
        const Luau::AstExprCall& call,
        Luau::WithPredicate<Luau::TypePackId>) override
    {
        std::optional<std::string> name = firstString(call);
        if (!name)
            return std::nullopt;

        std::optional<Luau::TypeId> type = classNamed(typeChecker.globalScope.get(), *name);
        if (!type)
        {
            typeChecker.reportError(Luau::TypeError{
                call.args.data[0]->location, Luau::UnknownSymbol{*name, Luau::UnknownSymbol::Type}});
            return std::nullopt;
        }

        Luau::TypeArena& arena = *typeChecker.currentModule->internalTypes;

        return answer(typeChecker, Luau::makeOption(typeChecker.builtinTypes, arena, *type));
    }

    bool infer(const Luau::MagicFunctionCallContext& context) override
    {
        std::optional<std::string> name = firstString(*context.callSite);
        if (!name)
            return false;

        std::optional<Luau::TypeId> type = classNamed(context.solver->rootScope.get(), *name);
        if (!type)
        {
            complain(context, Luau::UnknownSymbol{*name, Luau::UnknownSymbol::Type},
                context.callSite->args.data[0]->location);
            return false;
        }

        answer(context, Luau::makeOption(context.solver->builtinTypes, *context.solver->arena, *type));
        return true;
    }
};

// --- `instance:GetPropertyChangedSignal("Nope")` ----------------------------

/// A property name the class does not have is an error, not a string
struct MagicPropertyCheck final : Luau::MagicFunction
{
    std::optional<Luau::WithPredicate<Luau::TypePackId>> handleOldSolver(
        Luau::TypeChecker& typeChecker,
        const Luau::ScopePtr& scope,
        const Luau::AstExprCall& call,
        Luau::WithPredicate<Luau::TypePackId>) override
    {
        if (call.args.size != 1)
            return std::nullopt;

        std::optional<std::string> name = firstString(call);
        std::optional<Luau::TypeId> self = receiver(typeChecker, scope, call);
        if (!name || !self)
            return std::nullopt;

        const auto* type = Luau::get<Luau::ExternType>(Luau::follow(*self));
        if (type && !Luau::lookupExternTypeProp(type, *name))
            typeChecker.reportError(
                Luau::TypeError{call.args.data[0]->location, Luau::UnknownProperty{*self, *name}});

        return std::nullopt;
    }

    bool infer(const Luau::MagicFunctionCallContext& context) override
    {
        if (context.callSite->args.size != 1)
            return false;

        std::optional<std::string> name = firstString(*context.callSite);
        std::optional<Luau::TypeId> self = Luau::first(context.arguments);
        if (!name || !self)
            return false;

        const auto* type = Luau::get<Luau::ExternType>(Luau::follow(*self));
        if (type && !Luau::lookupExternTypeProp(type, *name))
            complain(context, Luau::UnknownProperty{*self, *name}, context.callSite->args.data[0]->location);

        return false;
    }
};

// --- `instance:QueryDescendants("Part, Model > Folder")` --------------------

/*
The class each group of a selector ends on.

A selector is comma separated groups, and each group is compounds joined
by `>` or `>>`. The class of a group is the bare capitalised name that
opens its last compound; a group that opens on `.`, `#`, `[` or `:` names
no class. This is the reading luau-lsp has, kept the same on purpose.
*/
std::vector<std::string> classesOfSelector(const std::string& selector)
{
    std::vector<std::string> out;
    std::vector<std::string> groups;

    int depth = 0;
    size_t start = 0;

    for (size_t i = 0; i < selector.size(); ++i)
    {
        char c = selector[i];

        if (c == '(')
            ++depth;
        else if (c == ')')
            --depth;
        else if (c == ',' && depth == 0)
        {
            groups.push_back(selector.substr(start, i - start));
            start = i + 1;
        }
    }

    groups.push_back(selector.substr(start));

    for (const std::string& group : groups)
    {
        depth = 0;
        size_t last = 0;

        for (size_t i = 0; i < group.size(); ++i)
        {
            char c = group[i];

            if (c == '(')
                ++depth;
            else if (c == ')')
                --depth;
            else if (c == '>' && depth == 0)
            {
                size_t next = i + 1;
                if (next < group.size() && group[next] == '>')
                    ++next;
                last = next;
            }
        }

        std::string compound = group.substr(last);
        size_t at = 0;

        while (at < compound.size() && (compound[at] == ' ' || compound[at] == '\t'))
            ++at;

        if (at >= compound.size() || compound[at] < 'A' || compound[at] > 'Z')
            continue;

        size_t end = at;
        while (end < compound.size()
               && (std::isalnum(static_cast<unsigned char>(compound[end])) || compound[end] == '_'))
            ++end;

        out.push_back(compound.substr(at, end - at));
    }

    return out;
}

/// A query answers with an array of the classes its selector names
struct MagicQueryDescendants final : Luau::MagicFunction
{
    template<typename Report>
    static std::optional<Luau::TypeId> array(
        const Luau::Scope* scope,
        Luau::TypeArena& arena,
        Luau::NotNull<Luau::BuiltinTypes> builtins,
        const std::string& selector,
        Report report)
    {
        std::vector<std::string> names = classesOfSelector(selector);
        if (names.empty())
            return std::nullopt;

        std::vector<Luau::TypeId> classes;

        for (const std::string& name : names)
        {
            std::optional<Luau::TypeId> type = classNamed(scope, name);
            if (!type)
            {
                report(name);
                return std::nullopt;
            }

            classes.push_back(*type);
        }

        Luau::TypeId element =
            classes.size() == 1 ? classes[0] : arena.addType(Luau::UnionType{std::move(classes)});

        return arena.addType(Luau::TableType{
            {}, Luau::TableIndexer{builtins->numberType, element}, Luau::TypeLevel{}, Luau::TableState::Sealed});
    }

    std::optional<Luau::WithPredicate<Luau::TypePackId>> handleOldSolver(
        Luau::TypeChecker& typeChecker,
        const Luau::ScopePtr&,
        const Luau::AstExprCall& call,
        Luau::WithPredicate<Luau::TypePackId>) override
    {
        std::optional<std::string> selector = firstString(call);
        if (!selector)
            return std::nullopt;

        Luau::TypeArena& arena = *typeChecker.currentModule->internalTypes;

        std::optional<Luau::TypeId> type = array(typeChecker.globalScope.get(), arena, typeChecker.builtinTypes,
            *selector, [&](const std::string& name) {
                typeChecker.reportError(Luau::TypeError{
                    call.args.data[0]->location, Luau::UnknownSymbol{name, Luau::UnknownSymbol::Type}});
            });

        if (!type)
            return std::nullopt;

        return answer(typeChecker, *type);
    }

    bool infer(const Luau::MagicFunctionCallContext& context) override
    {
        std::optional<std::string> selector = firstString(*context.callSite);
        if (!selector)
            return false;

        std::optional<Luau::TypeId> type = array(context.solver->rootScope.get(), *context.solver->arena,
            context.solver->builtinTypes, *selector, [&](const std::string& name) {
                complain(context, Luau::UnknownSymbol{name, Luau::UnknownSymbol::Type},
                    context.callSite->args.data[0]->location);
            });

        if (!type)
            return false;

        answer(context, *type);
        return true;
    }
};

// --- `parent:FindFirstChild("Markers")`, against the sourcemap --------------

/// The generated class of a type, when it is one
const Luau::ExternType* generated(Luau::TypeId type)
{
    const auto* external = Luau::get<Luau::ExternType>(Luau::follow(type));

    return isGeneratedInstance(external) ? external : nullptr;
}

/*
The child of one generated class, by the name the sourcemap gave it.

The sourcemap declares every instance as its own class, with one property
per child and a `Parent`. So the children of a class are its own
properties, less `Parent`, and a recursive search walks those in turn.
The nearest match wins, which is what the runtime answers too.
*/
std::optional<Luau::TypeId> childOf(Luau::TypeId self, const std::string& name, bool recursive)
{
    const Luau::ExternType* root = generated(self);
    if (!root)
        return std::nullopt;

    std::vector<const Luau::ExternType*> queue{root};
    std::set<const Luau::ExternType*> seen;

    for (size_t i = 0; i < queue.size(); ++i)
    {
        const Luau::ExternType* current = queue[i];
        if (!seen.insert(current).second)
            continue;

        for (const auto& [key, property] : current->props)
        {
            if (key == "Parent" || !property.readTy)
                continue;

            Luau::TypeId type = Luau::follow(*property.readTy);
            const Luau::ExternType* child = generated(type);
            if (!child)
                continue;

            if (key == name)
                return type;

            if (recursive)
                queue.push_back(child);
        }
    }

    return std::nullopt;
}

/// The parent of one generated class, when the sourcemap declared it
std::optional<Luau::TypeId> parentOf(const Luau::ExternType* type)
{
    auto parent = type->props.find("Parent");
    if (parent == type->props.end() || !parent->second.readTy)
        return std::nullopt;

    Luau::TypeId found = Luau::follow(*parent->second.readTy);

    if (!generated(found))
        return std::nullopt;

    return found;
}

/*
The ancestor of one generated class, by name.

A generated class does not carry its own name. Its parent does, as the
key the child sits under, so the name of an ancestor is read one level
up from it. The root has no parent, so it has no name here, and a
search for `game` by name answers nothing, which is what a script gets.
*/
std::optional<Luau::TypeId> ancestorOf(Luau::TypeId self, const std::string& name)
{
    const Luau::ExternType* current = generated(self);
    std::set<const Luau::ExternType*> seen;

    while (current && seen.insert(current).second)
    {
        std::optional<Luau::TypeId> candidate = parentOf(current);
        if (!candidate)
            return std::nullopt;

        const Luau::ExternType* candidateType = generated(*candidate);
        std::optional<Luau::TypeId> above = parentOf(candidateType);

        if (above)
        {
            const Luau::ExternType* aboveType = generated(*above);
            auto named = aboveType->props.find(name);

            if (named != aboveType->props.end() && named->second.readTy
                && Luau::follow(*named->second.readTy) == *candidate)
                return *candidate;
        }

        current = candidateType;
    }

    return std::nullopt;
}

/// The three lookups a sourcemap can answer
enum class Lookup
{
    Child,
    Wait,
    Ancestor
};

/*
A lookup by name answers with the sourcemap's class for that name.

`FindFirstChild` and `WaitForChild` read the children, and the former
reads every descendant when its second argument says so. `FindFirstAncestor`
walks the parents. A name the sourcemap does not hold leaves the declared
answer standing: `Instance?`, or `Instance` for a wait.
*/
struct MagicChildLookup final : Luau::MagicFunction
{
    Lookup lookup;

    explicit MagicChildLookup(Lookup lookup)
        : lookup(lookup)
    {
    }

    std::optional<Luau::TypeId> resolve(Luau::TypeId self, const Luau::AstExprCall& call) const
    {
        std::optional<std::string> name = firstString(call);
        if (!name)
            return std::nullopt;

        switch (lookup)
        {
        case Lookup::Ancestor:
            return ancestorOf(self, *name);

        case Lookup::Wait:
            return childOf(self, *name, false);

        case Lookup::Child:
        {
            bool recursive = false;

            if (call.args.size >= 2)
                if (auto* flag = call.args.data[1]->as<Luau::AstExprConstantBool>())
                    recursive = flag->value;

            return childOf(self, *name, recursive);
        }
        }

        return std::nullopt;
    }

    std::optional<Luau::WithPredicate<Luau::TypePackId>> handleOldSolver(
        Luau::TypeChecker& typeChecker,
        const Luau::ScopePtr& scope,
        const Luau::AstExprCall& call,
        Luau::WithPredicate<Luau::TypePackId>) override
    {
        std::optional<Luau::TypeId> self = receiver(typeChecker, scope, call);
        if (!self)
            return std::nullopt;

        std::optional<Luau::TypeId> found = resolve(*self, call);
        if (!found)
            return std::nullopt;

        return answer(typeChecker, *found);
    }

    bool infer(const Luau::MagicFunctionCallContext& context) override
    {
        std::optional<Luau::TypeId> self = Luau::first(context.arguments);
        if (!self)
            return false;

        std::optional<Luau::TypeId> found = resolve(*self, *context.callSite);
        if (!found)
            return false;

        answer(context, *found);
        return true;
    }
};

// --- attaching --------------------------------------------------------------

/// Every function of one type: itself, or each part of an overload set
std::vector<Luau::TypeId> overloadsOf(Luau::TypeId type)
{
    std::vector<Luau::TypeId> out;
    Luau::TypeId followed = Luau::follow(type);

    if (Luau::get<Luau::FunctionType>(followed))
        out.push_back(followed);
    else if (const auto* overloads = Luau::get<Luau::IntersectionType>(followed))
        for (Luau::TypeId part : overloads->parts)
            if (Luau::get<Luau::FunctionType>(Luau::follow(part)))
                out.push_back(Luau::follow(part));

    return out;
}

/// Put a handler on every overload of one function type
void attachEachOverload(Luau::TypeId type, const std::shared_ptr<Luau::MagicFunction>& magic)
{
    for (Luau::TypeId overload : overloadsOf(type))
        Luau::attachMagicFunction(overload, magic);
}

/// Put a handler and a completion tag on one method of a class
void attachMethod(
    Luau::ExternType* type,
    const char* name,
    const std::shared_ptr<Luau::MagicFunction>& magic,
    const char* tag = nullptr)
{
    auto found = type->props.find(name);
    if (found == type->props.end() || !found->second.readTy)
        return;

    attachEachOverload(*found->second.readTy, magic);

    /*
    The tag goes on each function, not on the set. `WaitForChild` is two
    overloads under one intersection, and a tag on the intersection is
    an assertion in Luau, not a tag.
    */
    if (tag)
        for (Luau::TypeId overload : overloadsOf(*found->second.readTy))
            Luau::attachTag(overload, tag);
}

/*
Under the new solver, `WaitForChild` is one function, not two overloads.

The definitions declare it twice, with and without a timeout, and the
new solver reads a handler only off a single function: an overload set
goes past the handler untouched, and a wait for a sourcemap child would
answer `Instance`. luau-lsp declares the one function for the new solver
and the two for the old, and this does the same.
*/
void singleWaitForChild(Luau::GlobalTypes& globals, Luau::ExternType* type, Luau::TypeId self)
{
    if (!FFlag::LuauSolverV2)
        return;

    auto found = type->props.find("WaitForChild");
    if (found == type->props.end() || !found->second.readTy)
        return;

    if (Luau::get<Luau::FunctionType>(Luau::follow(*found->second.readTy)))
        return;

    Luau::TypeId single = Luau::makeFunction(globals.globalTypes, self,
        {globals.builtinTypes->stringType, globals.builtinTypes->optionalNumberType}, {"name", "timeout"}, {self});

    Luau::Property replacement = Luau::Property::rw(single);
    replacement.documentationSymbol = found->second.documentationSymbol;

    found->second = replacement;
}

/*
Mark one library as Luau's, for the reference pages.

Roblox extends `debug` and `utf8`, so the definitions declare them under
`@roblox`. The pages for them live under `@luau`, where the base language
keeps them, and a card that asks under the wrong name gets no page.
*/
void fixLibrarySymbol(Luau::GlobalTypes& globals, const char* library)
{
    auto binding = globals.globalScope->bindings.find(globals.globalNames.names->getOrAdd(library));
    if (binding == globals.globalScope->bindings.end() || !binding->second.documentationSymbol)
        return;

    auto relabel = [](std::string symbol) {
        size_t at = symbol.find("@roblox");
        if (at != std::string::npos)
            symbol.replace(at, 7, "@luau");
        return symbol;
    };

    binding->second.documentationSymbol = relabel(*binding->second.documentationSymbol);

    Luau::TypeId type = binding->second.typeId;
    auto* mutableType = Luau::asMutable(type);

    if (mutableType->documentationSymbol)
        mutableType->documentationSymbol = relabel(*mutableType->documentationSymbol);

    if (auto* table = Luau::getMutable<Luau::TableType>(type))
    {
        table->name = std::string("typeof(") + library + ")";

        for (auto& [_, property] : table->props)
            if (property.documentationSymbol)
                property.documentationSymbol = relabel(*property.documentationSymbol);
    }
}
} // namespace

bool isGeneratedInstance(const Luau::ExternType* type)
{
    return type && Luau::startsWith(type->name, "_larvae_");
}

void attachRobloxMagic(Luau::GlobalTypes& globals)
{
    Luau::Scope* scope = globals.globalScope.get();

    fixLibrarySymbol(globals, "debug");
    fixLibrarySymbol(globals, "utf8");

    if (std::optional<Luau::TypeFun> object = scope->lookupType("Object"))
        if (auto* type = Luau::getMutable<Luau::ExternType>(object->type))
        {
            attachMethod(type, "IsA", std::make_shared<MagicInstanceIsA>(), "ClassNames");
            attachMethod(type, "GetPropertyChangedSignal", std::make_shared<MagicPropertyCheck>(), "Properties");
        }

    if (std::optional<Luau::TypeFun> instance = scope->lookupType("Instance"))
        if (auto* type = Luau::getMutable<Luau::ExternType>(instance->type))
        {
            Luau::attachTag(instance->type, Luau::kTypeofRootTag);

            auto whichIsA = std::make_shared<MagicFindFirstWhichIsA>();
            attachMethod(type, "FindFirstChildWhichIsA", whichIsA, "ClassNames");
            attachMethod(type, "FindFirstChildOfClass", whichIsA, "ClassNames");
            attachMethod(type, "FindFirstAncestorWhichIsA", whichIsA, "ClassNames");
            attachMethod(type, "FindFirstAncestorOfClass", whichIsA, "ClassNames");

            attachMethod(type, "Clone", std::make_shared<MagicInstanceClone>());

            auto propertyCheck = std::make_shared<MagicPropertyCheck>();
            attachMethod(type, "IsPropertyModified", propertyCheck, "Properties");
            attachMethod(type, "ResetPropertyToDefault", propertyCheck, "Properties");

            attachMethod(type, "QueryDescendants", std::make_shared<MagicQueryDescendants>());

            attachMethod(type, "FindFirstChild", std::make_shared<MagicChildLookup>(Lookup::Child), "Children");
            singleWaitForChild(globals, type, instance->type);
            attachMethod(type, "WaitForChild", std::make_shared<MagicChildLookup>(Lookup::Wait), "Children");
            attachMethod(type, "FindFirstAncestor", std::make_shared<MagicChildLookup>(Lookup::Ancestor));

            /*
            Every class under Instance takes Instance's metatable identity,
            so two instances of different classes compare with `==`. No
            class under Instance declares a metamethod of its own.
            */
            for (auto& [_, bound] : scope->exportedTypeBindings)
                if (auto* other = Luau::getMutable<Luau::ExternType>(bound.type))
                    if (other != type && Luau::isSubclass(other, type))
                        other->metatable = type->metatable;
        }

    if (std::optional<Luau::Binding> binding = scope->linearSearchForBinding("Instance", true))
        if (const auto* table = Luau::get<Luau::TableType>(Luau::follow(binding->typeId)))
        {
            auto fromExisting = table->props.find("fromExisting");
            if (fromExisting != table->props.end() && fromExisting->second.readTy)
                attachEachOverload(*fromExisting->second.readTy, std::make_shared<MagicInstanceFromExisting>());

            auto construct = table->props.find("new");
            if (construct != table->props.end() && construct->second.readTy)
                Luau::attachTag(*construct->second.readTy, "CreatableInstances");
        }

    if (std::optional<Luau::TypeFun> provider = scope->lookupType("ServiceProvider"))
        if (auto* type = Luau::getMutable<Luau::ExternType>(provider->type))
        {
            auto method = type->props.find("GetService");
            if (method != type->props.end() && method->second.readTy)
                Luau::attachTag(*method->second.readTy, "Services");
        }

    if (std::optional<Luau::TypeFun> item = scope->lookupType("EnumItem"))
        if (auto* type = Luau::getMutable<Luau::ExternType>(item->type))
            attachMethod(type, "IsA", std::make_shared<MagicEnumItemIsA>(), "Enums");
}

/*
`Vector3` and `vector` are one value at runtime, so they are one type here.

Luau declares `vector` for its own library, and the Roblox definitions
declare `Vector3` for the engine, and nothing ties them: `vector.magnitude`
refused a `Position`, and `vector.create` gave a thing no `CFrame` took.
Roblox has no such split. Its `Vector3` is the vector primitive, and the
vector library is documented against it.

The `vector` class takes every member of `Vector3` and its name, and the
`Vector3` type becomes a bound to it. Every reference in the definitions
then follows to the one class, and a hover reads `Vector3` either way.
`Vector2` stays apart: it is userdata at runtime, and the vector library
refuses it there too.
*/
void unifyVector3(Luau::GlobalTypes& globals)
{
    auto& types = globals.globalScope->exportedTypeBindings;

    auto vector = types.find("vector");
    auto vector3 = types.find("Vector3");
    if (vector == types.end() || vector3 == types.end())
        return;

    Luau::TypeId primitive = Luau::follow(vector->second.type);
    Luau::TypeId engine = Luau::follow(vector3->second.type);
    if (primitive == engine)
        return;

    auto* primitiveType = Luau::getMutable<Luau::ExternType>(primitive);
    auto* engineType = Luau::getMutable<Luau::ExternType>(engine);
    if (!primitiveType || !engineType)
        return;

    for (const auto& [name, property] : engineType->props)
        primitiveType->props[name] = property;

    primitiveType->name = engineType->name;
    primitiveType->definitionModuleName = engineType->definitionModuleName;
    primitiveType->definitionLocation = engineType->definitionLocation;

    if (engineType->metatable)
        primitiveType->metatable = engineType->metatable;

    Luau::asMutable(primitive)->documentationSymbol = Luau::asMutable(engine)->documentationSymbol;

    Luau::asMutable(engine)->ty.emplace<Luau::BoundType>(primitive);
    vector3->second.type = primitive;
}

std::vector<std::string> instanceClassNames(const Luau::GlobalTypes& globals)
{
    std::vector<std::string> out;

    std::optional<Luau::TypeFun> instance = globals.globalScope->lookupType("Instance");
    if (!instance)
        return out;

    const auto* root = Luau::get<Luau::ExternType>(instance->type);
    if (!root)
        return out;

    for (const auto& [_, bound] : globals.globalScope->exportedTypeBindings)
        if (const auto* type = Luau::get<Luau::ExternType>(bound.type))
            if (!isGeneratedInstance(type) && Luau::isSubclass(type, root))
                out.push_back(type->name);

    return out;
}

std::vector<std::string> enumNames(const Luau::GlobalTypes& globals)
{
    std::vector<std::string> out;

    auto enums = globals.globalScope->importedTypeBindings.find("Enum");
    if (enums == globals.globalScope->importedTypeBindings.end())
        return out;

    for (const auto& [name, _] : enums->second)
        out.push_back(name);

    return out;
}

std::vector<std::string> propertyNames(const Luau::ExternType* type)
{
    std::vector<std::string> out;

    while (type)
    {
        if (!isGeneratedInstance(type))
            for (const auto& [name, property] : type->props)
            {
                if (!property.readTy)
                    continue;

                Luau::TypeId value = Luau::follow(*property.readTy);
                if (Luau::get<Luau::FunctionType>(value) || Luau::isOverloadedFunction(value))
                    continue;

                const auto* table = Luau::get<Luau::TableType>(value);
                if (table && table->name && *table->name == "RBXScriptSignal")
                    continue;

                out.push_back(name);
            }

        type = type->parent ? Luau::get<Luau::ExternType>(Luau::follow(*type->parent)) : nullptr;
    }

    return out;
}

std::vector<std::string> childNames(const Luau::ExternType* type)
{
    std::vector<std::string> out;

    if (!isGeneratedInstance(type))
        return out;

    for (const auto& [name, property] : type->props)
        if (name != "Parent" && property.readTy && generated(*property.readTy))
            out.push_back(name);

    return out;
}
}
