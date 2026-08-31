/*
The shim over Luau's analysis frontend.

One session owns one Frontend, a file resolver that calls back into Rust,
and the text of every open module. Positions convert here and nowhere
else: Luau speaks lines and columns, the boundary speaks bytes, and the
session holds the line index of each module it opened.

The strings the shim returns live in per-session buffers that the next
call on the same session overwrites. Rust copies what it keeps, and the
session is used from one thread, which the Rust side guarantees.
*/

#include "shim.h"
#include "roblox.h"

/*
The layout the Rust side declares, checked where it is defined.

A field added to one of these structs and not to the other is a write past
the end of the caller's array. That is heap corruption, and heap corruption
surfaces somewhere else entirely: two crash reports for this shim landed in
an unrelated hash table and in a destructor. `analyzer.rs` asserts the same
numbers, so a one sided edit fails to build rather than shipping.
*/
static_assert(sizeof(LarvaeDiag) == 24, "LarvaeDiag must match RawDiag");
static_assert(sizeof(LarvaeCompletion) == 56, "LarvaeCompletion must match RawCompletion");
static_assert(sizeof(LarvaeLocation) == 24, "LarvaeLocation must match RawLocation");
static_assert(sizeof(LarvaeParameter) == 8, "LarvaeParameter must match RawParameter");
static_assert(sizeof(LarvaeSignature) == 24, "LarvaeSignature must match RawSignature");
static_assert(sizeof(LarvaeHint) == 24, "LarvaeHint must match RawHint");

static_assert(alignof(LarvaeDiag) == 8, "LarvaeDiag must align like RawDiag");
static_assert(alignof(LarvaeCompletion) == 8, "LarvaeCompletion must align like RawCompletion");
static_assert(alignof(LarvaeLocation) == 8, "LarvaeLocation must align like RawLocation");
static_assert(alignof(LarvaeParameter) == 8, "LarvaeParameter must align like RawParameter");
static_assert(alignof(LarvaeSignature) == 8, "LarvaeSignature must align like RawSignature");
static_assert(alignof(LarvaeHint) == 8, "LarvaeHint must align like RawHint");

#include "Luau/AstQuery.h"
#include "Luau/Scope.h"
#include "Luau/Autocomplete.h"
#include "Luau/ConfigResolver.h"
#include "Luau/BuiltinDefinitions.h"
#include "Luau/Frontend.h"
#include "Luau/ConstraintSolver.h"
#include "Luau/TypeInfer.h"
#include "Luau/ToString.h"
#include "Luau/TypeChecker2.h"
#include "Luau/TypeAttach.h"

#include "Luau/Common.h"
#include "Luau/ExperimentalFlags.h"

/* The compiler, for the bytecode listing. It shares Ast and Common with the
   analysis half and needs nothing else from the vendored build. */
#include "Luau/BytecodeBuilder.h"
#include "Luau/Compiler.h"
#include "Luau/ParseResult.h"
#include "Luau/PrettyPrinter.h"

#include <algorithm>
#include <cstdlib>
#include <cstring>
#include <map>
#include <utility>
#include <optional>
#include <string>
#include <vector>

namespace
{

struct RustFileResolver : Luau::FileResolver
{
    void* userdata = nullptr;
    larvae_resolve_fn resolve = nullptr;
    larvae_load_fn load = nullptr;
    std::map<std::string, std::string>* open;

    std::optional<Luau::SourceCode> readSource(const Luau::ModuleName& name) override
    {
        auto it = open->find(name);
        if (it != open->end())
            return Luau::SourceCode{it->second, Luau::SourceCode::Module};

        if (load)
        {
            const char* text = load(userdata, name.c_str());
            if (text)
                return Luau::SourceCode{std::string(text), Luau::SourceCode::Module};
        }

        return std::nullopt;
    }

    std::optional<Luau::ModuleInfo> resolveModule(
        const Luau::ModuleInfo* context, Luau::AstExpr* node, const Luau::TypeCheckLimits&) override
    {
        if (!resolve)
            return std::nullopt;

        if (auto* expr = node->as<Luau::AstExprConstantString>())
        {
            if (!context)
                return std::nullopt;

            std::string spec(expr->value.data, expr->value.size);
            const char* target = resolve(userdata, context->name.c_str(), spec.c_str());
            if (!target)
                return std::nullopt;

            return Luau::ModuleInfo{target};
        }

        /*
        The instance form arrives one hop at a time. The tracer resolves a
        chain innermost first and threads each answer into the next call as
        the context, so `script.Parent.Widget` is three calls, not one.

        A hop appends one segment to a symbolic spec: `\x01game` opens a
        chain at the DataModel, `\x01script\x02<path>` opens one at a
        file's own node, and every later segment follows one `\x01`. The
        resolver on the other side maps the chain through the mounts. A hop
        it can answer becomes that file's path; a hop it cannot stays
        symbolic, so a deeper hop can still finish the walk. No path and no
        require spec starts with that byte, so nothing collides.
        */
        if (auto* global = node->as<Luau::AstExprGlobal>())
        {
            if (global->name == "game")
                return Luau::ModuleInfo{"\x01game"};

            // The script's node is the module the tracer already names.
            if (global->name == "script" && context && !context->name.empty())
                return Luau::ModuleInfo{context->name};

            return std::nullopt;
        }

        std::string segment;

        if (auto* index = node->as<Luau::AstExprIndexName>())
        {
            segment = std::string(index->index.value);
        }
        else if (auto* index = node->as<Luau::AstExprIndexExpr>())
        {
            auto* key = index->index->as<Luau::AstExprConstantString>();
            if (!key)
                return std::nullopt;

            segment = std::string(key->value.data, key->value.size);
        }
        else if (auto* call = node->as<Luau::AstExprCall>())
        {
            auto* method = call->func->as<Luau::AstExprIndexName>();
            if (!method || call->args.size < 1)
                return std::nullopt;

            std::string name(method->index.value);
            if (name != "GetService" && name != "FindFirstChild" && name != "WaitForChild")
                return std::nullopt;

            auto* arg = call->args.data[0]->as<Luau::AstExprConstantString>();
            if (!arg)
                return std::nullopt;

            segment = std::string(arg->value.data, arg->value.size);
        }
        else
        {
            return std::nullopt;
        }

        if (!context || context->name.empty() || segment.empty())
            return std::nullopt;

        std::string spec = context->name[0] == '\x01'
            ? context->name
            : "\x01script\x02" + context->name;
        spec += '\x01';
        spec += segment;

        const char* target = resolve(userdata, context->name.c_str(), spec.c_str());
        if (target)
            return Luau::ModuleInfo{target};

        return Luau::ModuleInfo{spec};
    }
};

/* Byte offset <-> Luau Position, against one module's text. */
struct LineIndex
{
    std::vector<uint32_t> starts;

    explicit LineIndex(const std::string& text)
    {
        starts.push_back(0);
        for (uint32_t i = 0; i < text.size(); ++i)
            if (text[i] == '\n')
                starts.push_back(i + 1);
    }

    uint32_t byteOf(const Luau::Position& p, const std::string& text) const
    {
        uint32_t line = p.line < starts.size() ? starts[p.line] : (uint32_t)text.size();
        uint32_t byte = line + p.column;
        return byte > text.size() ? (uint32_t)text.size() : byte;
    }

    Luau::Position positionOf(uint32_t byte) const
    {
        uint32_t line = 0;
        for (size_t i = 0; i < starts.size(); ++i)
            if (starts[i] <= byte)
                line = (uint32_t)i;
            else
                break;
        return Luau::Position{line, byte - starts[line]};
    }
};

} // namespace

/*
The frontend keeps the type graphs it builds, or a hover has nothing to
find: the default discards a module's types the moment check returns.
*/
static Luau::FrontendOptions options()
{
    Luau::FrontendOptions o;
    o.retainFullTypeGraphs = true;
    return o;
}

/*
Set one Luau integer flag by name, because the linker cannot give the symbol.

The flag lives in the vendored archive and the shim is a separate shared
object, so naming the variable fails at the link. Luau registers every flag
in a list it walks itself, and the list is reachable from here.
*/
static void setLuauInt(const char* flag, int value)
{
    for (Luau::FValue<int>* it = Luau::FValue<int>::list; it; it = it->next)
    {
        if (strcmp(it->name, flag) == 0)
        {
            it->value = value;

            return;
        }
    }
}

/*
The two flag values a Luau language server needs, which luau-lsp sets too.

`LuauTarjanChildLimit` guards the type graph walk at 10000 children. The
Roblox type file crosses it, and Luau then refuses the whole file with one
error, `Code is too complex to typecheck`, reported at line 1 of 19728. So
`game` had no type, every Roblox completion came from nothing, and the load
failed quietly. 15000 is the number luau-lsp settles on for the same file.

`LuauTableTypeMaximumStringifierLength` cuts a rendered table type at 40
characters. That is Studio's number, chosen for a panel that cannot scroll.
An editor hover can, so zero lifts the cut and a table type reads in full.
*/
static void applyRequiredFlags()
{
    setLuauInt("LuauTarjanChildLimit", 15000);
    setLuauInt("LuauTableTypeMaximumStringifierLength", 0);
}

/*
Every Luau analysis flag on, minus the ones Luau marks experimental.

This is what luau-lsp does when its `fflags.enableByDefault` is on, and it
is on by default there. Luau ships a change behind a flag long before the
flag flips, so a language server that reads only the defaults reads an
older Luau than the one it links.

An experimental flag stays off. Luau names them for exactly this reason:
they are not ready to be read as behaviour.
*/
/*
What every boolean flag held before anything changed one.

The flags are global to the process, so a caller that turns them all on
changes what every later session infers. One snapshot, taken before the
first change, is what `larvae_reset_flags` puts back.
*/
static std::vector<std::pair<Luau::FValue<bool>*, bool>>& savedFlags()
{
    static std::vector<std::pair<Luau::FValue<bool>*, bool>> saved;

    if (saved.empty())
    {
        for (Luau::FValue<bool>* it = Luau::FValue<bool>::list; it; it = it->next)
            saved.push_back({it, it->value});
    }

    return saved;
}

static std::vector<std::pair<Luau::FValue<int>*, int>>& savedIntFlags()
{
    static std::vector<std::pair<Luau::FValue<int>*, int>> saved;

    if (saved.empty())
    {
        for (Luau::FValue<int>* it = Luau::FValue<int>::list; it; it = it->next)
            saved.push_back({it, it->value});
    }

    return saved;
}

/*
Snapshot before the first change, or there is nothing to put back.

Every path that writes a flag calls this first. The one that forgot was
`larvae_set_flag`, and the cost was a suite where the first flag test set
the new solver, the reset then took its snapshot from the changed world,
and every later session ran a solver nobody asked for.
*/
static void snapshotFlags()
{
    savedFlags();
    savedIntFlags();
}

static void enableAllFlags()
{
    snapshotFlags();

    for (Luau::FValue<bool>* it = Luau::FValue<bool>::list; it; it = it->next)
    {
        if (strncmp(it->name, "Luau", 4) == 0 && !Luau::isAnalysisFlagExperimental(it->name))
            it->value = true;
    }
}

/*
Whether the session runs the new solver, read once per question.

The new solver forces `forAutocomplete` off across the frontend, and its
autocomplete reads the main globals, so the second globals table is never
read at all. Everything that would fill it skips the work instead: the
builtin registration, the definition files, and the script bindings. On
this machine that is half the session build.
*/
static bool usingNewSolver()
{
    for (Luau::FValue<bool>* it = Luau::FValue<bool>::list; it; it = it->next)
        if (strcmp(it->name, "LuauSolverV2") == 0)
            return it->value;

    return false;
}

/*
The mode a module checks in, by what kind of file it is.

A plain Luau file keeps the platform default: nonstrict, and its own hot
comment wins over that as always. A worm-lowered module checks strict.
Its text is generated, so its "mode" was never authored, and nonstrict
butchered the types the worm worked to carry: a zero-parameter function
in a nonstrict module reads as `(...any) -> T`, because a nonstrict
function accepts anything. The data worm already wrote `--!strict` into
its lowering for exactly this reason; deciding it here covers every
resolving worm, and a hot comment in the source still wins.
*/
struct LarvaeConfigResolver : Luau::ConfigResolver
{
    Luau::Config defaultConfig;
    Luau::Config strictConfig;

    const Luau::Config& getConfig(const Luau::ModuleName& name, const Luau::TypeCheckLimits& limits) const override
    {
        (void)limits;

        const bool luau = name.size() >= 5 && name.compare(name.size() - 5, 5, ".luau") == 0;
        const bool lua = name.size() >= 4 && name.compare(name.size() - 4, 4, ".lua") == 0;

        return (luau || lua) ? defaultConfig : strictConfig;
    }
};

struct LarvaeSession
{
    RustFileResolver files;
    LarvaeConfigResolver configs;
    Luau::Frontend frontend;

    std::map<std::string, std::string> open;
    std::vector<std::string> diagStorage;
    std::vector<std::string> completionStorage;
    std::string hoverStorage;
    std::string documentationStorage;
    std::string locationStorage;
    std::string signatureStorage;
    std::vector<std::string> parameterStorage;
    std::vector<std::string> hintStorage;
    std::string bytecodeStorage;

    /* The comments of the last hover, rendered. See larvae_source_documentation. */
    std::string sourceDocumentationStorage;

    /*
    The text of each module a comment lookup read, for the length of one
    call. A completion list asks about hundreds of names, and every one of
    them would otherwise read its module off the disk again. The cache is
    cleared where a call starts, so no answer outlives the text it read.
    */
    std::map<std::string, std::string> commentText;

    /*
    The type arenas this session still owns, gathered where a call starts.
    See `ownedByLiveArena`, which is the reason they are gathered at all.
    */
    std::vector<const Luau::TypeArena*> liveArenas;

    /* The declared type of `script`, per module. See larvae_set_script_type. */
    std::map<std::string, std::string> scriptTypes;

    /*
    Which solver this session was built for, read once. The flag is set
    before the session is built and a later flip must not make the calls
    below disagree with what the constructor registered.
    */
    const bool newSolver = usingNewSolver();

    LarvaeSession()
        : frontend(&files, &configs, options())
    {
        files.open = &open;
        configs.defaultConfig.mode = Luau::Mode::Nonstrict;
        configs.strictConfig.mode = Luau::Mode::Strict;
        applyRequiredFlags();

        Luau::registerBuiltinGlobals(frontend, frontend.globals, false);
        Luau::freeze(frontend.globals.globalTypes);

        if (!newSolver)
        {
            Luau::registerBuiltinGlobals(frontend, frontend.globalsForAutocomplete, true);
            Luau::freeze(frontend.globalsForAutocomplete.globalTypes);
        }

        /*
        `script` is bound per module, because it names a different instance in
        every file. The frontend calls this as it builds the scope of a module,
        which is the only place that distinction can be made.

        The autocomplete pass reads its own copy of the globals, so the lookup
        follows the flag. A completion list and a hover that disagree about what
        `script` is would be worse than either being wrong alone.
        */
        frontend.prepareModuleScope = [this](const Luau::ModuleName& name, const Luau::ScopePtr& scope, bool forAutocomplete)
        {
            auto it = scriptTypes.find(name);
            if (it == scriptTypes.end())
                return;

            Luau::GlobalTypes& globals = forAutocomplete ? frontend.globalsForAutocomplete : frontend.globals;

            std::optional<Luau::TypeFun> declared = globals.globalScope->lookupType(it->second);
            if (!declared)
                return;

            /*
            A literal is enough for the name. Luau compares a global symbol by
            its text and hashes it the same way, so the pointer need not come
            from the name table of the module.
            */
            scope->bindings[Luau::AstName("script")] = Luau::Binding{declared->type, Luau::Location{}};
        };
    }
};

extern "C" {

void larvae_enable_all_flags(void)
{
    enableAllFlags();
}

void larvae_apply_required_flags(void)
{
    applyRequiredFlags();
}

/*
Put every boolean flag back to the value it had at startup.

The flags are global to the process and a session is built under whichever
ones were on. A caller that turns them all on therefore decides what every
later session in the same process infers, and the hover tests failed for
that reason alone. A caller that changes them puts them back with this.
*/
void larvae_reset_flags(void)
{
    snapshotFlags();

    for (auto& entry : savedFlags())
        entry.first->value = entry.second;

    for (auto& entry : savedIntFlags())
        entry.first->value = entry.second;

    applyRequiredFlags();
}

/*
One flag by name, from the text a project wrote.

Luau keeps a boolean list and an integer list, so the name decides which one
is asked. An unknown name is reported rather than ignored: a flag that Luau
renamed is a setting that silently stopped working, and the user is the only
one who can fix it.
*/
int larvae_set_flag(const char* name, const char* value)
{
    snapshotFlags();

    for (Luau::FValue<bool>* it = Luau::FValue<bool>::list; it; it = it->next)
    {
        if (strcmp(it->name, name) != 0)
            continue;

        if (strcmp(value, "true") == 0 || strcmp(value, "True") == 0)
            it->value = true;
        else if (strcmp(value, "false") == 0 || strcmp(value, "False") == 0)
            it->value = false;
        else
            return 2;

        return 0;
    }

    for (Luau::FValue<int>* it = Luau::FValue<int>::list; it; it = it->next)
    {
        if (strcmp(it->name, name) != 0)
            continue;

        char* end = nullptr;
        const long parsed = strtol(value, &end, 10);

        if (end == value || *end != '\0')
            return 2;

        it->value = static_cast<int>(parsed);

        return 0;
    }

    return 1;
}

LarvaeSession* larvae_session_new(void)
{
    return new LarvaeSession();
}

void larvae_session_free(LarvaeSession* s)
{
    delete s;
}

void larvae_set_resolver(LarvaeSession* s, void* userdata, larvae_resolve_fn resolve, larvae_load_fn load)
{
    s->files.userdata = userdata;
    s->files.resolve = resolve;
    s->files.load = load;
}

/*
`game:GetService("Players")` answers with `Players`, not with `Instance`.

The declaration says the method takes a string and gives an `Instance`,
because that is all a declaration can say. So every service a project binds
read as `Instance`, and a reader lost the whole type of the thing they had
just fetched.

Luau lets a function carry a magic handler that reads the call rather than
the signature. This one takes the string the author wrote, looks it up in
the type namespace, and answers with that type. luau-lsp attaches the same
thing to the same method for the same reason.

A name that is not a type is left alone, and the declared `Instance` stands.
Reporting it as an error belongs to the checker, not to a hover: a project
that fetches a service this build has no type for still deserves to run.
*/
/*
A call whose first argument names a type answers with that type.

The declaration says `GetService` takes a string and gives an `Instance`,
because that is all a declaration can say. So every service a project bound
read as `Instance`, and a reader lost the whole type of the thing they had
just fetched. `Instance.new("Part")` had the same hole.

Luau lets a function carry a magic handler that reads the call rather than
the signature. This one takes the string the author wrote, looks it up in
the type namespace, and answers with that type. luau-lsp attaches the same
thing to the same two functions for the same reason.

A name that is not a type is left alone, and the declared type stands.
Reporting it as an error belongs to the checker, not to a hover: a project
that fetches a service this build has no type for still deserves to run.
*/
struct MagicNamedType : Luau::MagicFunction
{
    /// Whether the type the name stands for is the kind this call answers with
    virtual bool accepts(Luau::TypeId) const
    {
        return true;
    }

    virtual std::optional<Luau::TypeId> named(Luau::Scope* scope, const Luau::AstExprCall& call) const
    {
        if (call.args.size < 1)
            return std::nullopt;

        auto text = call.args.data[0]->as<Luau::AstExprConstantString>();
        if (!text || !scope)
            return std::nullopt;

        std::string name(text->value.data, text->value.size);
        std::optional<Luau::TypeFun> found = scope->lookupType(name);

        // A generic type is not one of these, and substituting one needs arguments.
        if (!found || !found->typeParams.empty() || !found->typePackParams.empty())
            return std::nullopt;

        Luau::TypeId followed = Luau::follow(found->type);

        if (!accepts(followed))
            return std::nullopt;

        return followed;
    }

    std::optional<Luau::WithPredicate<Luau::TypePackId>> handleOldSolver(
        Luau::TypeChecker& typeChecker,
        const Luau::ScopePtr&,
        const Luau::AstExprCall& call,
        Luau::WithPredicate<Luau::TypePackId>) override
    {
        std::optional<Luau::TypeId> answer = named(typeChecker.globalScope.get(), call);
        if (!answer)
            return std::nullopt;

        Luau::TypeArena& arena = *typeChecker.currentModule->internalTypes;

        return Luau::WithPredicate<Luau::TypePackId>{arena.addTypePack({*answer})};
    }

    bool infer(const Luau::MagicFunctionCallContext& context) override
    {
        std::optional<Luau::TypeId> answer = named(context.solver->rootScope.get(), *context.callSite);
        if (!answer)
            return false;

        Luau::TypePackId pack = context.solver->arena->addTypePack({*answer});
        asMutable(context.result)->ty.emplace<Luau::BoundTypePack>(pack);

        return true;
    }
};

/*
`game:GetService("Players")`, which answers with the service.

The sourcemap speaks first. The tree declares `game` with one property
per service, and that property's type carries the children of the
project, so `GetService("ReplicatedStorage").Shared` resolves to the
folder the sourcemap holds. A service the tree does not list falls back
to its class, which is the answer the platform alone can give.
*/
struct MagicServiceLookup final : MagicNamedType
{
    std::optional<Luau::TypeId> named(Luau::Scope* scope, const Luau::AstExprCall& call) const override
    {
        if (scope && call.args.size >= 1)
        {
            if (auto text = call.args.data[0]->as<Luau::AstExprConstantString>())
            {
                std::string name(text->value.data, text->value.size);

                if (std::optional<Luau::Binding> game = scope->linearSearchForBinding("game", true))
                {
                    const auto* root = Luau::get<Luau::ExternType>(Luau::follow(game->typeId));

                    if (root)
                    {
                        auto prop = root->props.find(name);

                        if (prop != root->props.end() && prop->second.readTy
                            && Luau::get<Luau::ExternType>(Luau::follow(*prop->second.readTy)))
                            return Luau::follow(*prop->second.readTy);
                    }
                }
            }
        }

        return MagicNamedType::named(scope, call);
    }
};

/*
`Instance.new("Part")`, which answers with the class.

Only a class is an instance. `Instance.new("number")` names a type that is
not one, and the declared `Instance` stands, which is what the signature
promises and what the call really returns.
*/
struct MagicInstanceNew final : MagicNamedType
{
    bool accepts(Luau::TypeId ty) const override
    {
        return Luau::get<Luau::ExternType>(ty) != nullptr;
    }
};

/// Put a handler on one function that a global table holds under `name`.
static void attachTo(std::optional<Luau::TypeId> holder, const char* name, std::shared_ptr<Luau::MagicFunction> magic)
{
    if (!holder)
        return;

    const Luau::TableType* ttv = Luau::get<Luau::TableType>(Luau::follow(*holder));

    if (!ttv)
        return;

    auto found = ttv->props.find(name);

    if (found == ttv->props.end() || !found->second.readTy)
        return;

    if (!Luau::get<Luau::FunctionType>(Luau::follow(*found->second.readTy)))
        return;

    Luau::attachMagicFunction(*found->second.readTy, std::move(magic));
}

/// Put the handler on `ServiceProvider.GetService` of one global table.
static void attachServiceLookup(Luau::GlobalTypes& globals)
{
    std::optional<Luau::TypeFun> provider = globals.globalScope->lookupType("ServiceProvider");
    if (!provider)
        return;

    auto* ctv = Luau::getMutable<Luau::ExternType>(provider->type);
    if (!ctv)
        return;

    auto method = ctv->props.find("GetService");
    if (method == ctv->props.end() || !method->second.readTy)
        return;

    if (!Luau::get<Luau::FunctionType>(*method->second.readTy))
        return;

    Luau::attachMagicFunction(*method->second.readTy, std::make_shared<MagicServiceLookup>());
}

/*
Put the handler on `Instance.new` of one global table.

`Instance` is a value and not a type, so the binding is what carries the
table that holds `new`. The search is by text, because the name table of
the declaration file is not the one this call can reach.
*/
static void attachInstanceNew(Luau::GlobalTypes& globals)
{
    std::optional<Luau::Binding> binding = globals.globalScope->linearSearchForBinding("Instance", true);

    if (!binding)
        return;

    attachTo(binding->typeId, "new", std::make_shared<MagicInstanceNew>());
}

/*
The rig `Player.Character` carries, from the type larvae declares.

Roblox types the property as `Model?`, which knows no body part, so every
rig access in a project needs a cast. The project knows which rig it spawns,
and `larvaeTypes.d.luau` carries the two shapes. This swaps one in.

The property is not optional, and that is the decision this exists for.
`Model?` is the truthful type of a character that may not have spawned, and
it is also the reason nobody writes `player.Character.Humanoid`: the cast
that removes the question mark removes the parts with it. A project that
picks a rig has said what it wants, which is `local c: R15Character =
player.Character` and no cast. A character that is not there is a runtime
question, and the guard a reader writes for it reads the same either way.

Both the read type and the write type move, so an assignment to the
property takes the rig as well.
*/
static void applyCharacterType(Luau::GlobalTypes& globals, int kind)
{
    std::optional<Luau::TypeFun> r15 = globals.globalScope->lookupType("R15Character");
    std::optional<Luau::TypeFun> r6 = globals.globalScope->lookupType("R6Character");

    if (!r15 || !r6)
        return;

    std::optional<Luau::TypeId> rig;

    if (kind == 0)
        rig = Luau::follow(r15->type);
    else if (kind == 1)
        rig = Luau::follow(r6->type);
    else
        /*
        A place that allows both rigs gets the union, and the reader narrows
        it. `not_set` is the honest answer there: a name that only one rig
        has is an error until the code says which rig it holds.
        */
        rig = globals.globalTypes.addType(
            Luau::UnionType{{Luau::follow(r15->type), Luau::follow(r6->type)}});

    std::optional<Luau::TypeFun> player = globals.globalScope->lookupType("Player");

    if (!player)
        return;

    auto* ctv = Luau::getMutable<Luau::ExternType>(player->type);

    if (!ctv)
        return;

    auto character = ctv->props.find("Character");

    if (character == ctv->props.end())
        return;

    character->second.readTy = rig;
    character->second.writeTy = rig;
}

/*
Load one declaration file into the global scope.

The globals are frozen after the built in load, so they are thawed for this
and frozen again. The autocomplete globals take the same text, because a
completion list and a type check that disagree about what exists is worse
than either being wrong on its own.

Whether the big Roblox file loads at all is decided in `applyRequiredFlags`,
not here.
*/
int larvae_set_definitions(LarvaeSession* s, const char* name, const char* source)
{
    Luau::unfreeze(s->frontend.globals.globalTypes);

    Luau::LoadDefinitionFileResult result = s->frontend.loadDefinitionFile(
        s->frontend.globals, s->frontend.globals.globalScope, source, name, false, false);

    if (result.success && strcmp(name, "@roblox") == 0)
        Larvae::registerRobloxEnums(s->frontend.globals);

    attachServiceLookup(s->frontend.globals);
    attachInstanceNew(s->frontend.globals);

    Luau::freeze(s->frontend.globals.globalTypes);

    /*
    The second world exists for the old solver alone. There, a hover reads
    the autocomplete table, so both take the same text and the same
    handlers, or a card and a completion would disagree about what exists.
    The new solver reads the main table for both and the second load would
    be half the session build for nothing.
    */
    bool autocompleteOk = true;

    if (!s->newSolver)
    {
        Luau::unfreeze(s->frontend.globalsForAutocomplete.globalTypes);

        autocompleteOk = s->frontend
                             .loadDefinitionFile(s->frontend.globalsForAutocomplete,
                                 s->frontend.globalsForAutocomplete.globalScope, source, name, false, true)
                             .success;

        if (autocompleteOk && strcmp(name, "@roblox") == 0)
            Larvae::registerRobloxEnums(s->frontend.globalsForAutocomplete);

        attachServiceLookup(s->frontend.globalsForAutocomplete);
        attachInstanceNew(s->frontend.globalsForAutocomplete);

        Luau::freeze(s->frontend.globalsForAutocomplete.globalTypes);
    }

    if (!result.success && getenv("LARVAE_DEFS_DEBUG"))
    {
        for (const auto& e : result.parseResult.errors)
            fprintf(stderr, "defs %s:%d: %s\n", name, e.getLocation().begin.line + 1,
                    e.getMessage().c_str());

        if (result.parseResult.errors.empty())
        {
            if (result.module)
            {
                int shown = 0;

                for (const auto& e : result.module->errors)
                {
                    fprintf(stderr, "defs %s:%d: %s\n", name, e.location.begin.line + 1,
                            Luau::toString(e).c_str());

                    if (++shown >= 5)
                        break;
                }

                fprintf(stderr, "defs %s: %zu type error(s)\n", name, result.module->errors.size());
            }
            else
            {
                fprintf(stderr, "defs %s: no module came back\n", name);
            }
        }
    }

    return result.success && autocompleteOk ? 0 : 1;
}

void larvae_open(LarvaeSession* s, const char* path, const char* text)
{
    /*
    The same text is not a change.

    Every hover and completion opens the document before it asks, and a
    dirty mark forces the next check to rebuild the module and everything
    the answer reads. So a hover on an unchanged file re-checked it from
    nothing each time, which read as the types reloading for no reason. A
    real edit differs from the stored text and marks as before.
    */
    auto it = s->open.find(path);
    if (it != s->open.end() && it->second == text)
        return;

    s->open[path] = text;
    s->frontend.markDirty(path);
}

void larvae_invalidate(LarvaeSession* s, const char* path)
{
    std::vector<Luau::ModuleName> dependents;
    s->frontend.markDirty(path, &dependents);

    for (const auto& name : dependents)
        s->frontend.markDirty(name);
}

/*
Forget every `script` binding the sourcemap gave.

Each module that held one is marked dirty, because the type it was checked
against is about to be gone. A reload of the sourcemap is a change of what
every file's neighbours are.
*/
/*
Which rig `Player.Character` types to. 0 r15, 1 r6, 2 the union of both.

The setting changes while the session lives, so this re-applies: the
property is written again, over whatever the last call left. Every module
the session holds is marked dirty, because a module checked against the old
rig keeps the old answer until something asks it to check again.
*/
void larvae_set_character_type(LarvaeSession* s, int kind)
{
    Luau::unfreeze(s->frontend.globals.globalTypes);
    applyCharacterType(s->frontend.globals, kind);
    Luau::freeze(s->frontend.globals.globalTypes);

    // The second world exists for the old solver alone.
    if (!s->newSolver)
    {
        Luau::unfreeze(s->frontend.globalsForAutocomplete.globalTypes);
        applyCharacterType(s->frontend.globalsForAutocomplete, kind);
        Luau::freeze(s->frontend.globalsForAutocomplete.globalTypes);
    }

    /*
    A module that was already checked holds the answer the old rig gave. The
    caller may or may not republish a document after a config change, so the
    dirt is marked here and the next check reads the new type either way.
    */
    for (const auto& open : s->open)
        s->frontend.markDirty(open.first);
}

void larvae_clear_script_types(LarvaeSession* s)
{
    for (const auto& entry : s->scriptTypes)
        s->frontend.markDirty(entry.first);

    s->scriptTypes.clear();
}

/*
The declared type that `script` takes inside one module.

The name is a type the session already loaded through larvae_set_definitions.
A name the global scope does not hold binds nothing, and `script` then keeps
the type the platform gives it.
*/
void larvae_set_script_type(LarvaeSession* s, const char* path, const char* type_name)
{
    s->scriptTypes[path] = type_name;
    s->frontend.markDirty(path);
}

size_t larvae_check(LarvaeSession* s, const char* path, LarvaeDiag* out, size_t cap)
{
    Luau::CheckResult result;

    try
    {
        result = s->frontend.check(path);
    }
    catch (const std::exception&)
    {
        return 0;
    }

    auto it = s->open.find(path);
    if (it == s->open.end())
        return 0;

    LineIndex lines(it->second);

    s->diagStorage.clear();
    // Reserved up front: a push past capacity moves the strings, and the
    // pointers already written into `out` would dangle.
    s->diagStorage.reserve(cap);
    size_t n = 0;

    for (const Luau::TypeError& error : result.errors)
    {
        if (error.moduleName != path)
            continue;

        if (n >= cap)
            break;

        s->diagStorage.push_back(Luau::toString(error));

        out[n].start = lines.byteOf(error.location.begin, it->second);
        out[n].end = lines.byteOf(error.location.end, it->second);
        // Luau numbers its errors from 1000, and the number names the kind.
        out[n].code = error.code();
        out[n].severity = 1;
        out[n].message = s->diagStorage.back().c_str();
        ++n;
    }

    return n;
}

/*
The deprecated uses of one module, for the strikethrough in the editor.

Luau's own linter finds them: DeprecatedApi knows the platform's marks
and DeprecatedGlobal the language's, and both read the checked module,
so a member found through a type is found here. Nothing else of the
linter runs; larvae has its own lints and this asks one question.
*/
size_t larvae_deprecated(LarvaeSession* s, const char* path, LarvaeDiag* out, size_t cap)
{
    auto it = s->open.find(path);
    if (it == s->open.end())
        return 0;

    try
    {
        s->frontend.check(path);
    }
    catch (const std::exception&)
    {
        return 0;
    }

    Luau::ModulePtr module = s->frontend.moduleResolver.getModule(path);
    Luau::SourceModule* source = s->frontend.getSourceModule(path);
    if (!module || !source || !source->root)
        return 0;

    Luau::LintOptions options;
    options.enableWarning(Luau::LintWarning::Code_DeprecatedApi);
    options.enableWarning(Luau::LintWarning::Code_DeprecatedGlobal);

    std::vector<Luau::LintWarning> warnings;

    try
    {
        warnings = Luau::lint(
            source->root, *source->names, s->frontend.globals.globalScope, module.get(),
            source->hotcomments, options);
    }
    catch (const std::exception&)
    {
        return 0;
    }

    LineIndex lines(it->second);

    s->diagStorage.clear();
    s->diagStorage.reserve(cap);
    size_t n = 0;

    for (const Luau::LintWarning& warning : warnings)
    {
        if (n >= cap)
            break;

        s->diagStorage.push_back(warning.text);

        out[n].start = lines.byteOf(warning.location.begin, it->second);
        out[n].end = lines.byteOf(warning.location.end, it->second);
        out[n].code = 0;
        // 4 is Hint: the mark is a strikethrough, not a squiggle.
        out[n].severity = 4;
        out[n].message = s->diagStorage.back().c_str();
        ++n;
    }

    return n;
}

/*
Check one module the way a hover has to see it.

Luau's plain check runs in the mode the file asks for, and most Luau asks
for none, so an unannotated `local` is `any` and stays `any`. The
autocomplete checker runs strict whatever the file says, which is what makes
a require of a module that writes `local graph` come back as the table it
holds rather than as `any`. Luau says so on the option itself: strict mode,
in order to get more precise type information.

luau-lsp does the same thing for the same reason, and calls it `checkStrict`.
Measured on a real project: `local graph = require("../reactive/graph")` read
as `any` under the plain check and as the module's full table under this one.

The type graph is retained, because everything a hover reads lives in it. A
module checked once without it keeps an empty arena, so the module is marked
dirty first or the second check gives the same empty answer back.
*/
/*
The checked module for a path, wherever the solver put it.

The old solver keeps a second module set for autocomplete, and the hover
path reads that one for its retained type graphs. The new solver forces
`forAutocomplete` off inside the frontend, so everything lands in the main
resolver and the autocomplete set stays empty. Reading only the second set
answered every hover with nothing the moment a project turned the new
solver on, while completions kept working through Luau's own path.
*/
static Luau::ModulePtr checkedModule(LarvaeSession* s, const Luau::ModuleName& name)
{
    if (Luau::ModulePtr module = s->frontend.moduleResolverForAutocomplete.getModule(name))
        return module;

    return s->frontend.moduleResolver.getModule(name);
}

static Luau::ModulePtr strictCheck(LarvaeSession* s, const char* path)
{
    Luau::ModulePtr had = checkedModule(s, path);

    if (had && had->internalTypes->types.empty())
        s->frontend.markDirty(path);

    Luau::FrontendOptions options;
    options.retainFullTypeGraphs = true;
    options.forAutocomplete = true;
    options.runLintChecks = false;

    try
    {
        s->frontend.check(path, options);
    }
    catch (const std::exception& e)
    {
        // The silence hid a whole class of failure; the reason goes to the log.
        if (getenv("LARVAE_HOVER_DEBUG"))
            fprintf(stderr, "strictCheck %s: %s\n", path, e.what());

        return nullptr;
    }

    return checkedModule(s, path);
}

/*
The name a type carries, which is what goes in front of a method on a card.

A table takes the name of the alias that declared it, a metatable takes the
name of its own metatable, and a class takes its own. A type that carries
none answers nothing, and the caller keeps the path the author wrote.

Luau spells the synthetic name of a builtin table as `typeof(math)`, which
is not a name anybody wrote, so the wrapper comes off. luau-lsp reads the
name the same way, which is why `p:Destroy()` says `Instance` and
`function M.Init()` says the alias that names `M`.
*/
static std::optional<std::string> nameOfType(Luau::TypeId id)
{
    Luau::TypeId followed = Luau::follow(id);
    std::optional<std::string> name;

    if (const std::string* found = Luau::getName(followed))
        name = *found;
    else if (const Luau::MetatableType* mtv = Luau::get<Luau::MetatableType>(followed))
    {
        if (const std::string* found = Luau::getName(mtv->metatable))
            name = *found;
    }
    else if (const Luau::ExternType* etv = Luau::get<Luau::ExternType>(followed))
        name = etv->name;

    if (name && name->rfind("typeof(", 0) == 0 && !name->empty() && name->back() == ')')
        return name->substr(7, name->size() - 8);

    return name;
}

/// One property, and the type that carries it: an intersection needs both.
struct PropertyOfType
{
    Luau::TypeId base;
    Luau::Property property;
};

/*
Every read type one name stands for on a type, gathered.

A metatable keeps its methods behind `__index`, an intersection spreads them
over its parts, and a union has one per branch. The walk follows all three,
which is the shape a Luau module written with `setmetatable` takes, and it
is the walk luau-lsp does for the same question.

The seen list stops a cycle: `T.__index = T` points at itself.
*/
static void propertiesNamed(
    Luau::TypeId parent, const std::string& name, std::vector<Luau::TypeId>& seen, std::vector<PropertyOfType>& out)
{
    parent = Luau::follow(parent);

    if (std::find(seen.begin(), seen.end(), parent) != seen.end())
        return;

    seen.push_back(parent);

    if (const Luau::ExternType* etv = Luau::get<Luau::ExternType>(parent))
    {
        if (const Luau::Property* prop = Luau::lookupExternTypeProp(etv, name))
            out.push_back({parent, *prop});

        return;
    }

    if (const Luau::TableType* ttv = Luau::get<Luau::TableType>(parent))
    {
        auto prop = ttv->props.find(name);

        if (prop != ttv->props.end())
            out.push_back({parent, prop->second});

        return;
    }

    if (const Luau::MetatableType* mtv = Luau::get<Luau::MetatableType>(parent))
    {
        Luau::TypeId base = Luau::follow(mtv->table);

        // The table itself first, and the metatable only when it has nothing.
        if (const Luau::TableType* table = Luau::get<Luau::TableType>(base))
        {
            auto prop = table->props.find(name);

            if (prop != table->props.end())
            {
                out.push_back({base, prop->second});

                return;
            }
        }

        if (const Luau::TableType* meta = Luau::get<Luau::TableType>(Luau::follow(mtv->metatable)))
        {
            auto index = meta->props.find("__index");

            if (index != meta->props.end() && index->second.readTy)
            {
                Luau::TypeId through = Luau::follow(*index->second.readTy);

                if (through != parent
                    && (Luau::get<Luau::TableType>(through) || Luau::get<Luau::MetatableType>(through)))
                    propertiesNamed(through, name, seen, out);
            }
        }

        return;
    }

    if (const Luau::IntersectionType* itv = Luau::get<Luau::IntersectionType>(parent))
    {
        // The first part that has the name answers, because they are one type.
        for (Luau::TypeId part : itv->parts)
        {
            std::vector<PropertyOfType> one;

            propertiesNamed(part, name, seen, one);

            if (!one.empty())
            {
                out.insert(out.end(), one.begin(), one.end());

                return;
            }
        }

        return;
    }

    if (const Luau::UnionType* utv = Luau::get<Luau::UnionType>(parent))
    {
        for (Luau::TypeId option : utv->options)
            propertiesNamed(option, name, seen, out);
    }
}

/// Every read type one name stands for on a type, with the type that has it.
static std::vector<PropertyOfType> propertiesOf(Luau::TypeId parent, const std::string& name)
{
    std::vector<Luau::TypeId> seen;
    std::vector<PropertyOfType> found;

    propertiesNamed(parent, name, seen, found);

    return found;
}

/*
The type one property holds, when exactly one answers.

Two answers are not one card, so a union whose branches disagree says
nothing rather than picking a branch. luau-lsp draws the same line.
*/
static std::optional<Luau::TypeId> propertyOf(Luau::TypeId parent, const std::string& name)
{
    std::vector<PropertyOfType> found = propertiesOf(parent, name);

    if (found.size() != 1 || !found.front().property.readTy)
        return std::nullopt;

    return *found.front().property.readTy;
}

/// The text with every run of `find` removed, in place.
static void cutAll(std::string& text, const char* find)
{
    const size_t width = strlen(find);

    for (size_t at = text.find(find); at != std::string::npos; at = text.find(find, at))
        text.erase(at, width);
}

/// The text without the spaces and tabs at either end.
static std::string trimmed(const std::string& text)
{
    const size_t start = text.find_first_not_of(" \t\r\n");

    if (start == std::string::npos)
        return {};

    return text.substr(start, text.find_last_not_of(" \t\r\n") - start + 1);
}

/*
The name of a member, with the type it hangs off in front of it.

`p:Destroy()` reads as `Instance:Destroy()`, because the card is about the
method and every `Instance` has it. The receiver names it when its type
carries a name, and the path the author wrote stands when it does not:
`net.RecieveFull.Invoke` says where the function came from and no type does.

A colon hides the receiver from the argument list. The author never wrote
it, so showing it puts a parameter in the card that the source does not have.

A receiver the module recorded no type for loses its base, which is what
luau-lsp does: the card then carries the member and nothing in front of it.
*/
static std::string nameThrough(
    const Luau::ModulePtr& module, Luau::ToStringOptions& signature, Luau::AstExpr* receiver, char op, const std::string& member)
{
    const std::string suffix = (op == '\0' ? std::string() : std::string(1, op)) + member;

    if (op == ':')
        signature.hideFunctionSelfArgument = true;

    Luau::TypeId* parent = module->astTypes.find(receiver);

    if (!parent)
        return suffix;

    std::string base = trimmed(Luau::toString(receiver));

    if (std::optional<std::string> named = nameOfType(*parent))
        base = *named;

    return " " + base + suffix;
}

/*
Moonwave documentation, which is the comment block an author wrote.

luau-lsp reads the comments above a declaration and prints them as markdown,
with the moonwave tags turned into sections. Everything from here down to
`documentationOfType` is a port of its `DocumentationParser.cpp`, because a
card that formats the same prose differently is a card a reader has to read
twice.
*/

/// Whether the text opens with a prefix.
static bool beginsWith(const std::string& text, const char* prefix)
{
    return text.rfind(prefix, 0) == 0;
}

/// The text without the whitespace at its end.
static std::string trimmedEnd(const std::string& text)
{
    const size_t last = text.find_last_not_of(" \t\r\n");

    if (last == std::string::npos)
        return {};

    return text.substr(0, last + 1);
}

/// One string per line, with the separators dropped.
static std::vector<std::string> splitLines(const std::string& text)
{
    std::vector<std::string> lines;
    size_t at = 0;

    while (true)
    {
        const size_t stop = text.find('\n', at);

        if (stop == std::string::npos)
        {
            lines.push_back(text.substr(at));

            return lines;
        }

        lines.push_back(text.substr(at, stop - at));
        at = stop + 1;
    }
}

/*
The comments that belong to one node.

A comment belongs to a node when no blank line stands between them, and when
nothing else was declared in between. The walk finds the closest node before
the target and takes every comment after it, then keeps only the run that
touches the target line by line.
*/
struct AttachComments final : Luau::AstVisitor
{
    Luau::Position pos;
    std::vector<Luau::Comment> moduleComments;
    Luau::Position closestPreviousNode{0, 0};

    AttachComments(const Luau::Location& node, std::vector<Luau::Comment> comments)
        : pos(node.begin)
        , moduleComments(std::move(comments))
    {
    }

    std::vector<Luau::Comment> attached()
    {
        std::vector<Luau::Comment> candidates;

        for (const Luau::Comment& comment : moduleComments)
            if (comment.location.begin <= pos && comment.location.begin >= closestPreviousNode)
                candidates.push_back(comment);

        if (candidates.empty())
            return {};

        // Closest to the target first, so the run below reads outward.
        std::sort(candidates.begin(), candidates.end(),
            [](const Luau::Comment& a, const Luau::Comment& b)
            {
                return a.location.end > b.location.end;
            });

        std::vector<Luau::Comment> result;
        unsigned int adjacent = pos.line;

        for (const Luau::Comment& comment : candidates)
        {
            // A blank line between the comment and the node ends the block.
            if (comment.location.end.line + 1 < adjacent)
                break;

            result.push_back(comment);
            adjacent = comment.location.begin.line;
        }

        std::reverse(result.begin(), result.end());

        return result;
    }

    bool visit(Luau::AstExprTable* table) override
    {
        if (table->location.begin >= pos)
            return false;

        if (table->location.begin > closestPreviousNode)
            closestPreviousNode = table->location.begin;

        for (const Luau::AstExprTable::Item& item : table->items)
        {
            if (item.value->location.begin >= pos)
                continue;

            if (item.value->location.begin > closestPreviousNode)
                closestPreviousNode = item.value->location.begin;

            item.value->visit(this);

            if (item.value->location.end <= pos && item.value->location.end > closestPreviousNode)
                closestPreviousNode = item.value->location.end;
        }

        return false;
    }

    bool visit(Luau::AstTypeTable* table) override
    {
        if (table->location.begin >= pos)
            return false;

        if (table->location.begin > closestPreviousNode)
            closestPreviousNode = table->location.begin;

        for (const Luau::AstTableProp& prop : table->props)
        {
            if (prop.type->location.begin >= pos)
                continue;

            if (prop.type->location.begin > closestPreviousNode)
                closestPreviousNode = prop.type->location.begin;

            prop.type->visit(this);

            if (prop.type->location.end <= pos && prop.type->location.end > closestPreviousNode)
                closestPreviousNode = prop.type->location.end;
        }

        return false;
    }

    bool visit(Luau::AstStatDeclareExternType* declared) override
    {
        if (declared->location.begin >= pos)
            return false;

        if (declared->location.begin > closestPreviousNode)
            closestPreviousNode = declared->location.begin;

        for (const auto& prop : declared->props)
        {
            if (prop.ty->location.begin >= pos)
                continue;

            closestPreviousNode = std::max(closestPreviousNode, prop.ty->location.begin);
            prop.ty->visit(this);

            if (prop.ty->location.end <= pos)
                closestPreviousNode = std::max(closestPreviousNode, prop.ty->location.end);
        }

        return false;
    }

    bool visit(Luau::AstStatBlock* block) override
    {
        /*
        A block that ends before the position says nothing. A block that
        holds it cuts everything before its own start, because a comment
        outside the block belongs to whatever opened it.
        */
        if (block->location.begin >= pos)
            return false;

        if (block->location.begin > closestPreviousNode)
            closestPreviousNode = block->location.begin;

        for (Luau::AstStat* stat : block->body)
        {
            if (stat->location.begin >= pos)
                continue;

            stat->visit(this);

            if (stat->location.end <= pos && stat->location.end > closestPreviousNode)
                closestPreviousNode = stat->location.end;
        }

        return false;
    }

    // Types are skipped by default, and a declaration file is mostly types.
    bool visit(Luau::AstType*) override
    {
        return true;
    }

    bool visit(Luau::AstTypePack*) override
    {
        return true;
    }
};

/*
The type arenas this session still owns, gathered for one call.

`Luau::autocomplete` builds part of its answer in a `TypeArena` that it
destroys before it returns, so a completion entry can carry a type that no
longer exists. Rendering one reads a tag and stops, which is why that has
gone unnoticed. Reading where a function was declared follows an owning
string out of the type, and a wild pointer there is a crash the user sees:
two of them arrived as core dumps, one inside a hash table lookup with a
garbage key and one in the destructor of the entry map itself.

So a type is read only when the arena that holds it is one of these.
*/
static void gatherArenas(LarvaeSession* s)
{
    s->liveArenas.clear();
    s->liveArenas.push_back(&s->frontend.globals.globalTypes);
    s->liveArenas.push_back(&s->frontend.globalsForAutocomplete.globalTypes);

    for (const auto& [name, parsed] : s->frontend.sourceModules)
    {
        (void)parsed;

        for (const Luau::ModulePtr& module :
             {s->frontend.moduleResolver.getModule(name), s->frontend.moduleResolverForAutocomplete.getModule(name)})
        {
            if (!module)
                continue;

            s->liveArenas.push_back(&module->interfaceTypes);

            if (module->internalTypes)
                s->liveArenas.push_back(module->internalTypes.get());
        }
    }

    std::sort(s->liveArenas.begin(), s->liveArenas.end());
    s->liveArenas.erase(std::unique(s->liveArenas.begin(), s->liveArenas.end()), s->liveArenas.end());
}

/*
Whether a type sits in an arena the session still holds.

A type that names no arena is a builtin, ex: `number`. Those are persistent
and outlive every session, and they carry no declaration to read anyway, so
they answer no rather than passing through a lookup that says nothing.
*/
static bool ownedByLiveArena(LarvaeSession* s, Luau::TypeId ty)
{
    const Luau::TypeArena* arena = ty->owningArena;

    return arena && std::binary_search(s->liveArenas.begin(), s->liveArenas.end(), arena);
}

/// The text of one module, whether the editor holds it or the disk does.
static const std::string* moduleText(LarvaeSession* s, const Luau::ModuleName& name)
{
    auto open = s->open.find(name);

    if (open != s->open.end())
        return &open->second;

    auto cached = s->commentText.find(name);

    if (cached == s->commentText.end())
    {
        std::optional<Luau::SourceCode> loaded = s->files.readSource(name);

        // A module that does not load is cached as empty, so it loads once.
        cached = s->commentText.emplace(name, loaded ? std::move(loaded->source) : std::string()).first;
    }

    return cached->second.empty() ? nullptr : &cached->second;
}

/*
The comments above one node, normalised to the lines inside them.

A `--- ` line keeps its tail and a bare `---` becomes a blank line. A block
comment gives every line it holds, minus its two fence lines and minus the
indentation they share. A plain `-- ` line is not documentation and says
nothing, which is the rule luau-lsp follows and the one moonwave defines.
*/
static std::vector<std::string> commentsFor(LarvaeSession* s, const Luau::ModuleName name, const Luau::Location node)
{
    const Luau::SourceModule* source = s->frontend.getSourceModule(name);

    if (!source)
        return {};

    const std::string* text = moduleText(s, name);

    if (!text)
        return {};

    // A module that did not parse has no tree to walk and no comments in it.
    if (!source->root)
        return {};

    AttachComments walk(node, source->commentLocations);
    walk.visit(source->root);

    std::vector<Luau::Comment> found = walk.attached();

    if (found.empty())
        return {};

    LineIndex lines(*text);
    std::vector<std::string> comments;

    for (const Luau::Comment& comment : found)
    {
        // A comment the lexer could not close carries no prose.
        if (comment.type == Luau::Lexeme::Type::BrokenComment)
            continue;

        const uint32_t from = lines.byteOf(comment.location.begin, *text);
        const uint32_t to = lines.byteOf(comment.location.end, *text);

        if (to <= from)
            continue;

        const std::string whole = trimmed(text->substr(from, to - from));

        if (comment.type == Luau::Lexeme::Type::Comment)
        {
            if (beginsWith(whole, "--- "))
                comments.push_back(whole.substr(4));
            else if (whole == "---")
                comments.push_back("\n");

            continue;
        }

        if (comment.type != Luau::Lexeme::Type::BlockComment)
            continue;

        // The fence is `--[`, any number of `=`, then `[`.
        if (!beginsWith(whole, "--["))
            continue;

        size_t equals = 0;

        while (3 + equals < whole.size() && whole[3 + equals] == '=')
            ++equals;

        if (3 + equals >= whole.size() || whole[3 + equals] != '[')
            continue;

        const std::string opening = "--[" + std::string(equals, '=') + "[";
        const std::string closing = "]" + std::string(equals, '=') + "]";

        for (const std::string& line : splitLines(whole))
        {
            const std::string bare = trimmed(line);

            if (bare == opening || bare == closing)
                continue;

            comments.push_back(trimmedEnd(line));
        }

        /*
        The indentation the lines share comes off. A block written inside a
        function is indented in the file, and four of those spaces would
        make markdown read the whole block as a code fence.
        */
        size_t indent = std::string::npos;

        for (const std::string& line : comments)
        {
            if (line.empty())
                continue;

            indent = std::min(indent, line.find_first_not_of(" \n\r\t"));
        }

        for (std::string& line : comments)
        {
            if (line.empty())
                continue;

            line.erase(0, indent);
        }
    }

    return comments;
}

/*
The comments of one node as markdown, with the moonwave tags read.

A tag that names a section is gathered and printed under a heading, a tag
that is a flag becomes bold text of its own, and a line with no tag is
prose. The tags that describe the page rather than the thing, ex: `@within`,
say nothing to a reader hovering a name, so they are dropped.
*/
static std::string printMoonwave(const std::vector<std::string>& comments)
{
    if (comments.empty())
        return {};

    std::string result;
    std::vector<std::string> fields;
    std::vector<std::string> params;
    std::vector<std::string> returns;
    std::vector<std::string> throws;

    for (const std::string& comment : comments)
    {
        if (beginsWith(comment, "@param "))
            params.push_back(comment);
        else if (beginsWith(comment, "@return "))
            returns.push_back(comment);
        else if (beginsWith(comment, "@error "))
            throws.push_back(comment);
        else if (beginsWith(comment, "@field "))
            fields.push_back(comment);
        else if (comment == "@private")
            result += "**Private**\n";
        else if (comment == "@yields")
            result += "**Yields**\n";
        else if (comment == "@unreleased")
            result += "**Unreleased**\n";
        else if (comment == "@server")
            result += "**Server**\n";
        else if (comment == "@client")
            result += "**Client**\n";
        else if (comment == "@plugin")
            result += "**Plugin**\n";
        else if (comment == "@readonly")
            result += "**Read Only**\n";
        else if (beginsWith(comment, "@deprecated "))
        {
            result += "**Deprecated** ";

            std::string description = comment.substr(12);
            std::string version = description;

            if (const size_t space = description.find(' '); space != std::string::npos)
            {
                version = description.substr(0, space);
                description = description.substr(space);
            }

            if (version == description)
                result += "`" + version + "`\n";
            else
                result += "`" + version + "`" + description + "\n";
        }
        else if (beginsWith(comment, "@since "))
        {
            result += "**Since** `" + comment.substr(7) + "`\n";
        }
        else if (comment == "@ignore" || beginsWith(comment, "@tag ") || beginsWith(comment, "@within ")
                 || beginsWith(comment, "@class ") || beginsWith(comment, "@function ")
                 || beginsWith(comment, "@method ") || beginsWith(comment, "@prop ")
                 || beginsWith(comment, "@interface ") || beginsWith(comment, "@type ")
                 || beginsWith(comment, "@__index ") || beginsWith(comment, "@external "))
        {
            continue;
        }
        else
        {
            result += comment + "\n";
        }
    }

    /// One entry of a section: the name in code, then whatever followed it.
    auto named = [](const std::string& entry, size_t skip, const char* split) -> std::string
    {
        std::string tail = entry.substr(skip);
        std::string head = tail;

        if (const size_t at = split ? tail.find(split) : tail.find(' '); at != std::string::npos)
        {
            head = tail.substr(0, at);
            tail = tail.substr(at);
        }

        if (split)
            return (!head.empty() && head != tail) ? "\n- `" + head + "`" + tail : "\n- " + tail;

        return head == tail ? "\n- `" + head + "`" : "\n- `" + head + "`" + tail;
    };

    if (!fields.empty())
    {
        result += "\n\n**Fields**\n";

        for (const std::string& field : fields)
            result += named(field, 7, nullptr);
    }

    if (!params.empty())
    {
        result += "\n\n**Parameters**\n";

        for (const std::string& param : params)
            result += named(param, 7, nullptr);
    }

    if (!returns.empty())
    {
        result += "\n\n**Returns**\n";

        for (const std::string& one : returns)
            result += named(one, 8, " --");
    }

    if (!throws.empty())
    {
        result += "\n\n**Throws**\n";

        for (const std::string& one : throws)
            result += named(one, 7, " --");
    }

    return result;
}

/*
The comments above one location, rendered.

The name arrives by value on purpose. It often points into a type or into a
map that the lookup below writes to, and a reference to either is a
reference this call cannot promise will outlive it.
*/
static std::string documentationAt(LarvaeSession* s, Luau::ModuleName name, Luau::Location at)
{
    return printMoonwave(commentsFor(s, std::move(name), at));
}

/*
The documentation of a type, read where the type was declared.

A function carries the module and the line of its own declaration, a table
and a class carry theirs, and everything else carries none. That is the
whole of luau-lsp's `getDocumentationForType`.
*/
static std::string documentationOfType(LarvaeSession* s, Luau::TypeId ty)
{
    /*
    A type from an arena the session no longer holds is not read. See
    `ownedByLiveArena`: a completion entry can carry one, and following the
    declaration out of it is a wild pointer.
    */
    if (!ownedByLiveArena(s, ty))
        return {};

    const Luau::TypeId followed = Luau::follow(ty);

    if (followed != ty && !ownedByLiveArena(s, followed))
        return {};

    if (const Luau::FunctionType* ftv = Luau::get<Luau::FunctionType>(followed))
    {
        if (ftv->definition && ftv->definition->definitionModuleName)
            return documentationAt(s, *ftv->definition->definitionModuleName, ftv->definition->definitionLocation);

        return {};
    }

    if (const Luau::TableType* ttv = Luau::get<Luau::TableType>(followed))
    {
        if (!ttv->definitionModuleName.empty())
            return documentationAt(s, ttv->definitionModuleName, ttv->definitionLocation);

        return {};
    }

    if (const Luau::ExternType* etv = Luau::get<Luau::ExternType>(followed))
    {
        if (!etv->definitionModuleName.empty() && etv->definitionLocation)
            return documentationAt(s, etv->definitionModuleName, *etv->definitionLocation);
    }

    return {};
}

/*
The documentation of the node a cursor sits on.

A type reference reads the comment above the alias it names, whether that
alias sits in this module or in one it required. An alias declaration reads
the comment above itself. Nothing else answers here, which is the whole of
luau-lsp's `getDocumentationForAstNode`.
*/
static std::string documentationOfNode(
    LarvaeSession* s, const Luau::ModuleName& name, Luau::AstNode* node, const Luau::ScopePtr& scope)
{
    if (auto alias = node->as<Luau::AstStatTypeAlias>())
        return documentationAt(s, name, alias->location);

    auto ref = node->as<Luau::AstTypeReference>();

    if (!ref || !scope)
        return {};

    if (!ref->prefix)
    {
        // The scope chain holds where every alias in reach was declared.
        for (const Luau::Scope* walk = scope.get(); walk; walk = walk->parent.get())
        {
            auto found = walk->typeAliasLocations.find(ref->name.value);

            if (found != walk->typeAliasLocations.end())
                return documentationAt(s, name, found->second);
        }

        return {};
    }

    /*
    A prefixed name lives in another module, and the alias it points at is
    exported from there. The scope remembers which module the prefix stands
    for, which is the only way back to the file the comment is in.
    */
    for (const Luau::Scope* walk = scope.get(); walk; walk = walk->parent.get())
    {
        auto imported = walk->importedModules.find(ref->prefix->value);

        if (imported == walk->importedModules.end())
            continue;

        Luau::ModulePtr other = checkedModule(s, imported->second);

        if (!other)
            return {};

        auto binding = other->exportedTypeBindings.find(ref->name.value);

        if (binding == other->exportedTypeBindings.end() || !binding->second.definitionLocation)
            return {};

        return documentationAt(s, imported->second, *binding->second.definitionLocation);
    }

    return {};
}

/*
The innermost node that holds a position, types included.

luau-lsp answers a hover from this node and from nothing else, so three
shapes it covers had no card here at all: the `type` keyword of an alias,
the name of a property inside a table type, and a type that is not a
reference, ex: the `"Strength"` of a union of literals.

The span test is half open, which is what luau-lsp uses: the position that
sits at the end of one node belongs to the next.
*/
struct NodeAtPosition final : Luau::AstVisitor
{
    Luau::Position position;
    Luau::AstNode* best = nullptr;

    explicit NodeAtPosition(Luau::Position position)
        : position(position)
    {
    }

    bool visit(Luau::AstNode* node) override
    {
        if (!node->location.contains(position))
            return false;

        // Smallest wins, so a name inside a table type beats the table.
        if (!best || best->location.encloses(node->location))
            best = node;

        return true;
    }

    // Types and packs are skipped by default, and they are half the question.
    bool visit(Luau::AstType* node) override
    {
        return visit(static_cast<Luau::AstNode*>(node));
    }

    bool visit(Luau::AstTypePack* node) override
    {
        return visit(static_cast<Luau::AstNode*>(node));
    }

    // A generic parameter is a declaration and not a type the cursor asks about.
    bool visit(Luau::AstGenericType*) override
    {
        return false;
    }

    bool visit(Luau::AstGenericTypePack*) override
    {
        return false;
    }
};

/*
The innermost type reference that holds a position.

The ancestry walk is the obvious way to find one and it does not reach every
type: a return type inside `(number, string)` sits in a pack the walk does
not enter, so hovering it showed the whole function instead. A visitor
reaches every type node the file has, and the smallest match is the one the
cursor is on.
*/
struct TypeAtPosition final : Luau::AstVisitor
{
    Luau::Position position;
    Luau::AstTypeReference* found = nullptr;

    explicit TypeAtPosition(Luau::Position position)
        : position(position)
    {
    }

    bool visit(Luau::AstType* node) override
    {
        auto ref = node->as<Luau::AstTypeReference>();

        if (ref && ref->location.containsClosed(position))
        {
            // Smallest wins, so a generic argument beats the type that holds it.
            if (!found || found->location.encloses(ref->location))
                found = ref;
        }

        return true;
    }

    /*
    A return type sits in a pack, and the base visitor does not enter one.
    That is why a return annotation showed the whole function: the walk
    reached the pack and stopped at its door.
    */
    bool visit(Luau::AstTypePack*) override
    {
        return true;
    }

    // Types hide inside expressions and statements, so every node is walked.
    bool visit(Luau::AstStat*) override
    {
        return true;
    }

    bool visit(Luau::AstExpr*) override
    {
        return true;
    }
};

/*
The hover card, in the shape a reader expects to see.

Three things decide whether a hover is useful, and the first cut of this got
all three wrong.

It asked `findTypeAtPosition`, which answers for an expression. A local's
declaration is not an expression, so hovering the name a reader just wrote
gave nothing at all: `local total = add(1, 2)` had a type and would not show
it. `findExprOrLocalAtPosition` answers for both, and a local's type comes
from the scope that holds it.

It printed a function as its type, `(number, number) -> number`. A reader
wants the signature they wrote, with the name and the argument names, and
Luau renders that itself through `toStringNamedFunction`.

It printed a local as a bare type. A card that reads `local total: number`
says what the line is as well as what it holds, which is what luau-lsp shows
and what a reader is looking for.

The options match luau-lsp's, because the two servers should not disagree
about how one type reads.
*/
const char* larvae_hover(LarvaeSession* s, const char* path, uint32_t byte, int show_table_kinds,
                         int include_string_length)
{
    auto it = s->open.find(path);
    if (it == s->open.end())
        return nullptr;

    /*
    The comments of the last hover are the last hover's. A card that answers
    nothing must not leave the previous card's prose behind it, and one call
    reads one view of every module it touches.
    */
    s->sourceDocumentationStorage.clear();
    s->commentText.clear();

    Luau::ModulePtr module = strictCheck(s, path);
    const Luau::SourceModule* source = s->frontend.getSourceModule(path);
    if (!module || !source)
        return nullptr;

    /*
    After the check, because a check builds the modules whose arenas these
    are. Gathering them first left the set empty on the first hover of a
    session, and every type then read as one this session does not own.
    */
    gatherArenas(s);

    LineIndex lines(it->second);
    Luau::Position position = lines.positionOf(byte);

    /*
    A comment holds prose and prose has no type.

    Every lookup below answers for the innermost node that contains the
    position, and a comment inside a table constructor is contained by that
    constructor. So hovering any word of a doc comment showed the type of
    the table it stood above, on every word, which is noise where a reader
    expects nothing. luau-lsp asks Luau the same question first.
    */
    if (Luau::isWithinComment(*source, position))
        return nullptr;

    Luau::ScopePtr scope = Luau::findScopeAtPosition(*module, position);
    Luau::ExprOrLocal found = Luau::findExprOrLocalAtPosition(*source, position);

    std::optional<Luau::TypeId> type;
    std::string aliasName;
    // The parameters of the alias, so the card reads `type Entity<T = nil>`.
    std::optional<Luau::TypeFun> aliasParameters;

    /*
    Where the comment block that documents the answer sits, when the type
    itself does not say. A property of a table knows its own line, a local
    knows where it was declared, and neither is reachable from the type.
    */
    std::optional<std::pair<Luau::ModuleName, Luau::Location>> documentationLocation;

    /*
    A type name answers with what it stands for.

    `type Point = { x: number }` and every later `Point` both read as the
    alias, so hovering either shows the shape the name hides. The type
    namespace is separate from the value namespace, so the scope is asked a
    different question here.

    The node the position sits in decides which question is asked at all,
    and the innermost one wins. luau-lsp reads a hover the same way, which
    is why the `T` inside `Signal<T...>` answers nothing rather than
    answering for the `Signal` that holds it.
    */
    std::vector<Luau::AstNode*> ancestry =
        Luau::findAstAncestryOfPosition(*source, position, /* includeTypes = */ true);

    NodeAtPosition at(position);
    source->root->visit(&at);

    if (scope && at.best)
    {
        if (auto ref = at.best->as<Luau::AstTypeReference>())
        {
            std::optional<Luau::TypeFun> fun = ref->prefix
                ? scope->lookupImportedType(ref->prefix->value, ref->name.value)
                : scope->lookupType(ref->name.value);

            if (fun)
            {
                aliasName = ref->prefix
                    ? std::string(ref->prefix->value) + "." + ref->name.value
                    : std::string(ref->name.value);
                aliasParameters = *fun;
                type = fun->type;
            }
        }
        else if (auto alias = at.best->as<Luau::AstStatTypeAlias>())
        {
            // The `type` keyword of a declaration hovers the alias it opens.
            if (std::optional<Luau::TypeFun> fun = scope->lookupType(alias->name.value))
            {
                aliasName = alias->name.value;
                aliasParameters = *fun;
                type = fun->type;
            }
        }
    }

    // A local is not an expression, so the scope answers for it.
    if (!type)
    if (Luau::AstLocal* local = found.getLocal())
    {
        if (scope)
            type = scope->lookup(local);

        documentationLocation = {path, local->location};
    }

    /*
    A local on the left of an assignment answers from the scope too.

    `SendSize = Save.Size` records no type for the name it writes to, so
    every lookup below fell through and the card showed the function the
    line stands in. The scope knows what the local is, which is what the
    reader is asking.
    */
    if (!type && scope)
    {
        if (Luau::AstExpr* expr = found.getExpr())
        {
            if (auto local = expr->as<Luau::AstExprLocal>())
                type = scope->lookup(local->local);
        }
    }

    /*
    A local read as an expression carries the line it was declared on, so a
    reader hovering a use sees the comment written above the declaration.
    */
    if (!documentationLocation)
    {
        if (Luau::AstExpr* expr = found.getExpr())
        {
            if (auto local = expr->as<Luau::AstExprLocal>(); local && local->local)
                documentationLocation = {path, local->local->location};
        }
    }

    /*
    A key in a table constructor answers with what the field holds.

    `Cancel = function() ... end` records the key as a string, so hovering
    the name a reader wrote showed `string (6 bytes)` and not the function
    it stands for. luau-lsp reads the value the key names.
    */
    if (!type)
    {
        for (auto up = ancestry.rbegin(); up != ancestry.rend(); ++up)
        {
            auto table = (*up)->as<Luau::AstExprTable>();

            if (!table)
                continue;

            for (const Luau::AstExprTable::Item& item : table->items)
            {
                if (!item.key || !item.key->location.contains(position))
                    continue;

                if (Luau::TypeId* held = module->astTypes.find(item.value))
                    type = *held;

                break;
            }

            break;
        }
    }

    /*
    A field or a method reached through a dot.

    `findTypeAtPosition` answers for the expression that starts at the
    position, and the name in `a.b` starts after the dot, so `b` answered
    with nothing. The property the name stands for is what a reader hovering
    `b` is asking about, and the type in front of the dot holds it.
    */
    // Kept for the card's name: the index expression the position sits in.
    Luau::AstExprIndexName* hovered_index = nullptr;

    for (auto up = ancestry.rbegin(); up != ancestry.rend(); ++up)
    {
        auto index = (*up)->as<Luau::AstExprIndexName>();

        /*
        Only the name after the separator, and not the receiver in front of
        it. Hovering `game` in `game:GetService()` asks about `game`, and
        answering with the signature of `GetService` answers a question the
        reader did not ask.
        */
        if (!index || !index->indexLocation.containsClosed(position))
            continue;

        hovered_index = index;

        /*
        The property, and not the type the module recorded for the whole
        index expression.

        `table.create<V>(count, value)` reads as `table.create(count:
        number, value: nil)` at a call site, because what the module
        recorded is the instantiated type. A reader hovering the name wants
        the signature they can call, generics and all, and the declaration
        of the property is that. The same lookup answers for a name a
        statement assigns: `function M.Init()` records nothing under
        `M.Init`, and the type of `M` carries the property. luau-lsp reads
        both from here.
        */
        if (Luau::TypeId* parent = module->astTypes.find(index->expr))
        {
            std::vector<PropertyOfType> properties
                = propertiesOf(Luau::follow(*parent), index->index.value);

            if (!properties.empty())
            {
                const PropertyOfType& first = properties.front();

                if (!type && properties.size() == 1 && first.property.readTy)
                    type = *first.property.readTy;

                /*
                Where the property was written, which is the only handle a
                card has on the comment above it. The inferred line comes
                first and the annotated line second, as luau-lsp reads them.
                */
                if (std::optional<Luau::ModuleName> where = Luau::getDefinitionModuleName(first.base))
                {
                    if (first.property.location)
                        documentationLocation = {*where, *first.property.location};
                    else if (first.property.typeLocation)
                        documentationLocation = {*where, *first.property.typeLocation};
                }
            }
        }

        if (!type)
        {
            if (auto recorded = module->astTypes.find(index))
                type = *recorded;
        }

        break;
    }

    if (!type)
        type = Luau::findTypeAtPosition(*module, *source, position);

    /*
    A global the module recorded no type for answers from the scope.

    `function set(key)` writes to a global, and the write records nothing
    under the name, so hovering the name a reader had just written answered
    nothing. The scope holds what the name is, and luau-lsp reads it there.
    */
    if (!type && scope)
    {
        if (Luau::AstExpr* expr = found.getExpr())
        {
            if (auto global = expr->as<Luau::AstExprGlobal>())
                type = scope->lookup(global->name);
        }
    }

    /*
    The node the position sits in, when every lookup above answers nothing.

    luau-lsp reads a hover off the innermost node, so three shapes it
    covers had no card here at all: the `type` keyword of an alias, the
    name of a property inside a table type, and a type that is not a
    reference, ex: the `"Strength"` of a union of literals.
    */
    if (!type)
    {
        if (Luau::AstNode* node = at.best)
        {
            if (auto table = node->as<Luau::AstTypeTable>())
            {
                if (Luau::TypeId* resolved = module->astResolvedTypes.find(table))
                {
                    type = *resolved;

                    // On one of the property names, the property answers.
                    for (const Luau::AstTableProp& prop : table->props)
                    {
                        if (!prop.location.containsClosed(position))
                            continue;

                        const Luau::TypeId whole = Luau::follow(*resolved);

                        if (std::optional<Luau::ModuleName> where = Luau::getDefinitionModuleName(whole))
                            documentationLocation = {*where, prop.location};

                        if (std::optional<Luau::TypeId> held = propertyOf(whole, prop.name.value))
                            type = *held;

                        break;
                    }
                }
            }
            else if (Luau::AstType* written = node->asType())
            {
                if (Luau::TypeId* resolved = module->astResolvedTypes.find(written))
                    type = *resolved;
            }
        }
    }

    /*
    The function the position stands inside, when nothing smaller answers.

    A reader hovering `if`, `end`, or the name in `function M.Init()` is
    inside a function and on nothing that carries a type of its own. luau-lsp
    answers with the function itself, so the card still says what the reader
    is looking at rather than nothing at all.
    */
    // True when the card is about the function the position stands inside.
    bool from_enclosing_function = false;

    if (!type)
    {
        for (auto up = ancestry.rbegin(); up != ancestry.rend(); ++up)
        {
            auto fn = (*up)->as<Luau::AstExprFunction>();

            if (!fn || !fn->location.containsClosed(position))
                continue;

            if (auto found = module->astTypes.find(fn))
            {
                type = *found;
                from_enclosing_function = true;
            }

            break;
        }
    }

    if (!type)
        return nullptr;

    /*
    The comments that document the answer, in the order luau-lsp reads them.

    The reference page comes first and Rust looks that up, so this is the
    rest of the chain: where the type itself was declared, then the alias
    the cursor names, then the line the walk above found. The first one that
    says anything wins, and the answer waits in the session for the call
    that asks for it.
    */
    {
        std::string documentation = documentationOfType(s, *type);

        if (documentation.empty() && at.best)
            documentation = documentationOfNode(s, path, at.best, scope);

        if (documentation.empty() && documentationLocation)
            documentation = documentationAt(s, documentationLocation->first, documentationLocation->second);

        s->sourceDocumentationStorage = documentation;
    }

    Luau::ToStringOptions opts;
    opts.exhaustive = true;
    opts.useLineBreaks = true;
    opts.functionTypeArguments = true;
    opts.hideNamedFunctionTypeParameters = false;
    opts.scope = scope;

    /*
    `{| x: number |}` says the table is sealed, which matters to somebody
    writing a type and to nobody reading one. luau-lsp hides it by default
    for that reason, and a project that wants it turns it back on.
    */
    opts.hideTableKind = show_table_kinds == 0;

    Luau::TypeId followed = Luau::follow(*type);

    /*
    A type name says it is a type, and what it stands for.

    It comes before every other shape, including a function, because
    `type Callback = () -> ()` is an alias first: a card that opened with
    `function(` answered a question the reader did not ask. luau-lsp reads
    the alias first for the same reason.
    */
    if (!aliasName.empty())
    {
        /*
        The parameters of the alias come with the name.

        `type Entity = { __T: T }` says nothing about where `T` comes from,
        and the alias is generic: `type Entity<T = nil>` is the line the
        author wrote and the one a reader is looking for.
        */
        std::string parameters;

        const bool generic = aliasParameters
            && (!aliasParameters->typeParams.empty() || !aliasParameters->typePackParams.empty());

        if (generic)
        {
            Luau::ToStringOptions bare;

            parameters = "<";
            bool written = false;

            for (const Luau::GenericTypeDefinition& param : aliasParameters->typeParams)
            {
                if (written)
                    parameters += ", ";

                parameters += Luau::toString(Luau::follow(param.ty), bare);

                if (param.defaultValue)
                    parameters += " = " + Luau::toString(Luau::follow(*param.defaultValue), bare);

                written = true;
            }

            /*
            A pack parameter counts as well. `type Signal<T... = ...any>` is
            the line the author wrote, and an alias that lists only its type
            parameters answered `type Signal`, which is a different alias.
            */
            for (const Luau::GenericTypePackDefinition& param : aliasParameters->typePackParams)
            {
                if (written)
                    parameters += ", ";

                parameters += Luau::toString(Luau::follow(param.tp), bare);

                if (param.defaultValue)
                    parameters += " = " + Luau::toString(Luau::follow(*param.defaultValue), bare);

                written = true;
            }

            parameters += ">";
        }

        s->hoverStorage = "type " + aliasName + parameters + " = " + Luau::toString(followed, opts);

        return s->hoverStorage.c_str();
    }

    /*
    A function shows its signature. The name comes from whichever half of
    the answer carries one, and a function with no name still renders, so an
    anonymous one is not left blank.
    */
    if (const Luau::FunctionType* ftv = Luau::get<Luau::FunctionType>(followed))
    {
        /*
        A signature prints a named type by its name.

        The card for a value expands every type it holds, because that is
        what a reader hovering a value wants to see. A signature is the
        other case: `self: World` and `component: Component<a>` say what
        the parameter is, and the whole of `World` inlined says it worse.
        The line breaks go too, because a signature is one line. luau-lsp
        splits the two the same way.
        */
        Luau::ToStringOptions signature;
        signature.functionTypeArguments = true;
        signature.hideNamedFunctionTypeParameters = false;
        signature.hideTableKind = opts.hideTableKind;
        signature.scope = scope;

        // The expression the position sits in, whichever lookup answered.
        Luau::AstExpr* expr = found.getExpr();

        if (!expr && hovered_index)
            expr = hovered_index;

        /*
        The enclosing function is not the name under the cursor.

        A card that took the type of the function a keyword stands in and
        the name of whatever the cursor was on read as a function that does
        not exist. The signature goes out unnamed, which is what luau-lsp
        shows.
        */
        if (from_enclosing_function)
            expr = nullptr;

        // The whole card up to the arguments, ex: `function Instance:Destroy`.
        std::string head = "function";

        std::optional<Luau::AstName> written = from_enclosing_function
            ? std::nullopt
            : found.getName();

        /*
        A position inside a type has no expression under it, and luau-lsp
        still writes the space it would put before a name: the card for a
        field of a table type reads `function (self: T)`. The enclosing
        function is the other case, and that one carries no space.
        */
        if (!written && !expr && !from_enclosing_function && !found.getLocal())
            head += " ";

        if (written)
        {
            head += " ";
            head += written->value;
        }
        else if (expr)
        {
            if (auto local = expr->as<Luau::AstExprLocal>())
            {
                head += " ";
                head += local->local->name.value;
            }
            else if (auto global = expr->as<Luau::AstExprGlobal>())
            {
                head += " ";
                head += global->name.value;
            }
            else if (auto index = expr->as<Luau::AstExprIndexName>())
            {
                head += nameThrough(module, signature, index->expr, index->op,
                    index->index.value);
            }
            else if (auto index = expr->as<Luau::AstExprIndexExpr>())
            {
                head += nameThrough(module, signature, index->expr, '\0',
                    "[" + Luau::toString(index->index) + "]");
            }
        }

        /*
        The `function` keyword goes in front, because the card should read
        like the line the author would write. Luau renders the rest.
        */
        std::string rendered = Luau::toStringNamedFunction("", *ftv, signature);

        /*
        Luau writes an argument the declaration did not name as `_: number`,
        and a card shows the type alone. luau-lsp cuts the same text for the
        same reason.
        */
        cutAll(rendered, "_: ");

        s->hoverStorage = head + rendered;

        return s->hoverStorage.c_str();
    }

    std::string text = Luau::toString(followed, opts);

    /*
    A string literal says how long it is.

    A reader hovering `"Loaded"` already knows it is a string. The length is
    the thing they cannot count at a glance, and it is what luau-lsp shows.
    The character count comes too when it differs from the byte count, which
    is the case that matters: a name with an accent in it.
    */
    if (include_string_length)
    {
        if (Luau::AstExpr* expr = found.getExpr())
        {
            if (auto literal = expr->as<Luau::AstExprConstantString>())
            {
                const size_t bytes = literal->value.size;
                size_t characters = 0;

                for (size_t i = 0; i < bytes; ++i)
                {
                    // A continuation byte is part of the character before it.
                    if ((static_cast<unsigned char>(literal->value.data[i]) & 0xC0) != 0x80)
                        characters++;
                }

                s->hoverStorage = "string (" + std::to_string(bytes) + " bytes";

                if (characters != bytes)
                    s->hoverStorage += ", " + std::to_string(characters) + " characters";

                s->hoverStorage += ")";

                return s->hoverStorage.c_str();
            }
        }
    }

    /*
    A global says it is a type and what it stands for.

    `Color3` is a table of constructors, and a card that opened with `{
    fromHSV: ...` read as though the reader were hovering a value they had
    made. luau-lsp writes `type Color3 = ...`, which says where the name
    came from.
    */
    if (Luau::AstExpr* expr = found.getExpr())
    {
        if (auto global = expr->as<Luau::AstExprGlobal>())
        {
            s->hoverStorage = "type " + std::string(global->name.value) + " = " + text;

            return s->hoverStorage.c_str();
        }
    }

    // A local says what the line is, as well as what it holds.
    if (Luau::AstLocal* local = found.getLocal())
    {
        s->hoverStorage = "local " + std::string(local->name.value) + ": " + text;

        return s->hoverStorage.c_str();
    }

    if (Luau::AstExpr* expr = found.getExpr())
    {
        if (auto localExpr = expr->as<Luau::AstExprLocal>())
        {
            s->hoverStorage = "local " + std::string(localExpr->local->name.value) + ": " + text;

            return s->hoverStorage.c_str();
        }
    }

    s->hoverStorage = text;

    return s->hoverStorage.c_str();
}

/*
The comments that document the last hover, as markdown.

The hover computes it, because it already knows which node the cursor is on
and what type answered, and a second walk would have to find both again. The
answer belongs to the session until the next hover on that session, which is
the same rule every other string here follows.
*/
const char* larvae_source_documentation(LarvaeSession* s)
{
    return s->sourceDocumentationStorage.empty() ? nullptr : s->sourceDocumentationStorage.c_str();
}

/*
Fill a location from a module name and a Luau span.

The path string lives on the session, which is the same rule every other
string here follows: it stays valid until the next call and Rust copies
what it keeps.
*/
static int fillLocation(LarvaeSession* s, const std::string& module, const Luau::Location& at, LarvaeLocation* out)
{
    s->locationStorage = module;

    out->path = s->locationStorage.c_str();
    out->start_line = at.begin.line;
    out->start_character = at.begin.column;
    out->end_line = at.end.line;
    out->end_character = at.end.column;

    return 1;
}

/*
Where the name at a position is declared.

Three shapes answer, and they cover what a reader clicks. A local resolves
to its own declaration, which the AST carries outright. A require resolves
to the module it names, because a click on the string is a click on the
file. Everything else asks the type: a function or a table field carries the
location it was defined at, which is how a click on a method reaches the
module that wrote it.

A name with no answer returns 0 rather than a guess. A wrong jump costs a
reader more than no jump, because they have to work out where they landed.
*/
int larvae_definition(LarvaeSession* s, const char* path, uint32_t byte, LarvaeLocation* out)
{
    auto it = s->open.find(path);
    if (it == s->open.end())
        return 0;

    try
    {
        s->frontend.check(path);
    }
    catch (const std::exception&)
    {
        return 0;
    }

    Luau::ModulePtr module = s->frontend.moduleResolver.getModule(path);
    const Luau::SourceModule* source = s->frontend.getSourceModule(path);
    if (!module || !source)
        return 0;

    LineIndex lines(it->second);
    Luau::Position position = lines.positionOf(byte);

    std::vector<Luau::AstNode*> ancestry = Luau::findAstAncestryOfPosition(*source, position);
    if (ancestry.empty())
        return 0;

    Luau::AstNode* node = ancestry.back();

    // A local names its own declaration, and the AST already holds it.
    if (auto* local = node->as<Luau::AstExprLocal>())
        return fillLocation(s, path, local->local->location, out);

    /*
    A click inside a require string opens the module it names. The resolver
    answered that question once already, and the module map holds the
    answer, so nothing here re-resolves a path.
    */
    if (auto* text = node->as<Luau::AstExprConstantString>())
    {
        for (Luau::AstNode* up : ancestry)
        {
            auto* call = up->as<Luau::AstExprCall>();
            if (!call)
                continue;

            auto* callee = call->func->as<Luau::AstExprGlobal>();
            if (!callee || callee->name != "require")
                continue;

            if (!s->files.resolve)
                return 0;

            std::string spec(text->value.data, text->value.size);
            const char* target = s->files.resolve(s->files.userdata, path, spec.c_str());
            if (!target)
                return 0;

            return fillLocation(s, target, Luau::Location{}, out);
        }
    }

    /*
    Everything else asks the type where it came from. A function type
    carries its definition location, which reaches a method or a field
    declared in another module.
    */
    std::optional<Luau::TypeId> type = Luau::findTypeAtPosition(*module, *source, position);
    if (!type)
        return 0;

    if (auto* fn = Luau::get<Luau::FunctionType>(Luau::follow(*type)))
    {
        if (fn->definition)
        {
            // The module name is absent for a function this file declares.
            const std::string& where = fn->definition->definitionModuleName
                ? *fn->definition->definitionModuleName
                : std::string(path);

            return fillLocation(s, where, fn->definition->definitionLocation, out);
        }
    }

    return 0;
}

/*
Where the type of the name at a position is declared.

This is the other jump a reader wants: not the value, the shape of it. A
table type carries the location it was defined at, and a class type carries
its own.
*/
int larvae_type_definition(LarvaeSession* s, const char* path, uint32_t byte, LarvaeLocation* out)
{
    auto it = s->open.find(path);
    if (it == s->open.end())
        return 0;

    try
    {
        s->frontend.check(path);
    }
    catch (const std::exception&)
    {
        return 0;
    }

    Luau::ModulePtr module = s->frontend.moduleResolver.getModule(path);
    const Luau::SourceModule* source = s->frontend.getSourceModule(path);
    if (!module || !source)
        return 0;

    LineIndex lines(it->second);
    std::optional<Luau::TypeId> type = Luau::findTypeAtPosition(*module, *source, lines.positionOf(byte));
    if (!type)
        return 0;

    Luau::TypeId followed = Luau::follow(*type);

    if (auto* table = Luau::get<Luau::TableType>(followed))
    {
        // An empty name means the table was written here, not imported.
        const std::string& where =
            table->definitionModuleName.empty() ? std::string(path) : table->definitionModuleName;

        if (table->definitionLocation != Luau::Location{})
            return fillLocation(s, where, table->definitionLocation, out);
    }

    return 0;
}


/*
The signature of the call that encloses a position.

The walk goes outward from the position to the nearest call, because a
caret inside an argument is still inside the call that takes it. Luau gives
the callee's type, and a function type carries its argument pack and the
names the author wrote, so the label is built from the type and not from the
source text. A callee with no function type has no signature to show.

The active parameter counts the commas the caret has passed. That is what
the editor bolds, and it is why an incomplete call still answers: the author
is mid-typing, which is exactly when the help is wanted.
*/
int larvae_signature_help(
    LarvaeSession* s, const char* path, uint32_t byte,
    LarvaeSignature* sig, LarvaeParameter* out, size_t cap)
{
    auto it = s->open.find(path);
    if (it == s->open.end())
        return 0;

    try
    {
        s->frontend.check(path);
    }
    catch (const std::exception&)
    {
        return 0;
    }

    Luau::ModulePtr module = s->frontend.moduleResolver.getModule(path);
    const Luau::SourceModule* source = s->frontend.getSourceModule(path);
    if (!module || !source)
        return 0;

    LineIndex lines(it->second);
    Luau::Position position = lines.positionOf(byte);

    std::vector<Luau::AstNode*> ancestry = Luau::findAstAncestryOfPosition(*source, position);

    Luau::AstExprCall* call = nullptr;
    for (auto node = ancestry.rbegin(); node != ancestry.rend(); ++node)
    {
        if (auto* found = (*node)->as<Luau::AstExprCall>())
        {
            call = found;
            break;
        }
    }

    if (!call)
        return 0;

    auto* callee = module->astTypes.find(call->func);
    if (!callee)
        return 0;

    const Luau::FunctionType* fn = Luau::get<Luau::FunctionType>(Luau::follow(*callee));
    if (!fn)
        return 0;

    Luau::ToStringOptions opts;
    opts.exhaustive = false;
    opts.maxTypeLength = 200;

    auto [args, tail] = Luau::flatten(fn->argTypes);

    s->parameterStorage.clear();

    /*
    A method call passes the receiver as the first argument, and the author
    did not write it. To show it would put the caret on the wrong parameter
    for every call.
    */
    size_t first = (call->self && !args.empty()) ? 1 : 0;

    for (size_t i = first; i < args.size(); ++i)
    {
        std::string label;

        if (i < fn->argNames.size() && fn->argNames[i])
            label = fn->argNames[i]->name + ": ";

        label += Luau::toString(args[i], opts);
        s->parameterStorage.push_back(label);
    }

    /*
    Only a variadic the author wrote shows. The new solver gives every
    function an implicit tail pack, and printing that drew `, ...` on
    every signature: a one-argument function read as taking more, and a
    no-argument function read as `(...)`.
    */
    if (tail)
    {
        const auto* variadic = Luau::get<Luau::VariadicTypePack>(Luau::follow(*tail));

        if (variadic && !variadic->hidden)
            s->parameterStorage.push_back("...: " + Luau::toString(variadic->ty, opts));
    }

    std::string label = "(";
    for (size_t i = 0; i < s->parameterStorage.size(); ++i)
    {
        if (i)
            label += ", ";
        label += s->parameterStorage[i];
    }
    label += "): " + Luau::toString(fn->retTypes, opts);

    s->signatureStorage = label;

    // The caret has passed one argument for each argument that ends before it.
    uint32_t active = 0;
    for (Luau::AstExpr* arg : call->args)
    {
        if (arg->location.end < position)
            active++;
    }

    sig->label = s->signatureStorage.c_str();
    sig->active = active;
    sig->count = s->parameterStorage.size();

    size_t n = s->parameterStorage.size() < cap ? s->parameterStorage.size() : cap;
    for (size_t i = 0; i < n; ++i)
        out[i].label = s->parameterStorage[i].c_str();

    return 1;
}

/*
Collect the inlay hints of one module.

Four kinds, the same four luau-lsp draws. A local with no annotation gets
its inferred type after the name, and a loop variable gets the same before
the `in`. A parameter gets its type, a function gets its return type after
the parameter list, and a call site gets the name of each parameter before
the argument it receives.

The walk visits the AST rather than the type graph, because a hint belongs
at a place in the text, and only the AST knows where the author wrote a
name. A binding that carries an annotation is skipped: the type is on the
screen already, and to repeat it is noise.
*/
struct HintCollector : Luau::AstVisitor
{
    LarvaeSession* session;
    Luau::ModulePtr module;
    std::vector<std::pair<Luau::Position, std::pair<std::string, uint8_t>>> found;

    /*
    Which sites the project asked for. Both kinds render as a type hint of
    the protocol, so only the collector can tell a local's hint from a
    parameter's, and the setting has to be answered here.
    */
    bool wantVariables = true;
    bool wantParameters = true;
    bool wantReturns = false;

    /* 0 none, 1 the literal arguments, 2 every argument. luau-lsp's modes. */
    int nameMode = 0;

    Luau::ToStringOptions opts;

    HintCollector(LarvaeSession* s, Luau::ModulePtr m)
        : session(s)
        , module(std::move(m))
    {
        /*
        The server truncates the label for display; the whole type still
        crosses, because the accept edit writes it into the file. The cap
        stays for the pathological shapes nobody would accept anyway.
        */
        opts.exhaustive = false;
        opts.maxTypeLength = 2000;
    }

    /*
    The rendered type, or nothing when the hint would not help.

    Luau keys its type map by expression, and a local is not an expression,
    so the type comes from what the local was given: the value in a `local`
    statement, and the argument pack of the enclosing function for a
    parameter. That is the same answer by a different road.
    */
    std::optional<std::string> render(Luau::TypeId type)
    {
        std::string text = Luau::toString(Luau::follow(type), opts);

        // A type nobody can act on is not worth the space it takes.
        if (text.empty() || text == "any" || text == "*error-type*")
            return std::nullopt;

        return text;
    }

    bool visit(Luau::AstStatLocal* node) override
    {
        if (!wantVariables)
            return true;

        /*
        The types come from the scope, the way luau-lsp reads them. The
        value expression cannot answer for every binding: one call fills
        several variables, and the call is one expression with one entry
        in the type map, so `local buf, rest = f()` hinted the first name
        and went quiet on the rest. The old solver's check retains no
        scopes at all, and each binding falls back to its value there.
        */
        Luau::ScopePtr scope = Luau::findScopeAtPosition(*module, node->location.begin);

        for (size_t i = 0; i < node->vars.size; ++i)
        {
            Luau::AstLocal* local = node->vars.data[i];

            // An annotation puts the type on screen already.
            if (!local || local->annotation)
                continue;

            // A discard is a discard. luau-lsp leaves `_` bare too.
            if (strcmp(local->name.value, "_") == 0)
                continue;

            std::optional<Luau::TypeId> type = scope ? scope->lookup(local) : std::nullopt;

            /*
            The scope speaks first and the value expression covers what it
            cannot say. The old solver's plain check keeps no scopes, and
            its nonstrict scope binds a local as `any` even where the
            value's type is known, so a scope answer that renders to
            nothing hands over rather than going quiet.
            */
            std::optional<std::string> text = type ? render(*type) : std::nullopt;

            if (!text && i < node->values.size)
                if (auto* fallback = module->astTypes.find(node->values.data[i]))
                    text = render(Luau::follow(*fallback));


            if (!text)
                continue;

            /*
            A function written out in place carries its whole signature on
            screen already, so a hint would repeat the line it sits on.
            luau-lsp skips the same case.
            */
            if (i < node->values.size && node->values.data[i]->is<Luau::AstExprFunction>()
                && Luau::get<Luau::FunctionType>(Luau::follow(*type)))
                continue;

            /*
            Luau names a table type after the binding that holds it, so
            `const EmptyStats = { ... }` rendered as `EmptyStats`. A hint
            that repeats the variable's own name says nothing.
            */
            if (*text == local->name.value)
                continue;

            found.push_back({local->location.end, {": " + *text, 1}});
        }

        return true;
    }

    /*
    The loop variables of a `for ... in`, before the `in`.

    The types come from the scope, because a loop variable is a local and
    not an expression, and the iterator decides what it holds.
    */
    bool visit(Luau::AstStatForIn* node) override
    {
        if (!wantVariables)
            return true;

        for (size_t i = 0; i < node->vars.size; ++i)
        {
            Luau::AstLocal* var = node->vars.data[i];

            if (!var || var->annotation)
                continue;

            if (strcmp(var->name.value, "_") == 0)
                continue;

            Luau::ScopePtr scope = Luau::findScopeAtPosition(*module, var->location.begin);
            if (!scope)
                continue;

            std::optional<Luau::TypeId> type = scope->lookup(var);
            if (!type)
                continue;

            if (auto text = render(*type))
            {
                if (*text == var->name.value)
                    continue;

                found.push_back({var->location.end, {": " + *text, 1}});
            }
        }

        return true;
    }

    bool visit(Luau::AstExprFunction* node) override
    {
        if (!wantParameters && !wantReturns)
            return true;

        auto* self = module->astTypes.find(node);
        if (!self)
            return true;

        const Luau::FunctionType* fn = Luau::get<Luau::FunctionType>(Luau::follow(*self));
        if (!fn)
            return true;

        if (wantParameters)
        {
            auto [args, tail] = Luau::flatten(fn->argTypes);
            (void)tail;

            // A method takes the receiver first, and the author did not write it.
            size_t offset = node->self ? 1 : 0;

            for (size_t i = 0; i < node->args.size; ++i)
            {
                Luau::AstLocal* arg = node->args.data[i];

                if (!arg || arg->annotation)
                    continue;

                size_t index = i + offset;
                if (index >= args.size())
                    break;

                if (auto text = render(args[index]))
                    found.push_back({arg->location.end, {": " + *text, 1}});
            }
        }

        /*
        The return type, after the parameter list. `()` still hints: a
        reader asking what a call gives back is told plainly that it gives
        nothing, which luau-lsp answers the same way.
        */
        if (wantReturns && !node->returnAnnotation && node->argLocation)
        {
            std::string text = Luau::toString(fn->retTypes, opts);

            if (!text.empty() && text.find("*error-type*") == std::string::npos)
                found.push_back({node->argLocation->end, {": " + text, 1}});
        }

        return true;
    }

    /*
    The name of each parameter, before the argument that fills it.

    An argument that is itself named like the parameter is skipped, the
    way luau-lsp skips it: `add(entity, x)` against `add(entity: Entity)`
    already reads as what it is, and a hint would say the word twice.
    */
    /// Case folded, for the name compare luau-lsp makes the same way.
    static bool sameWord(const std::string& a, const char* b)
    {
        for (size_t i = 0; i < a.size(); ++i)
        {
            if (!b[i] || tolower((unsigned char)a[i]) != tolower((unsigned char)b[i]))
                return false;
        }

        return b[a.size()] == '\0';
    }

    bool visit(Luau::AstExprCall* node) override
    {
        if (nameMode == 0)
            return true;

        const Luau::TypeId* callee = module->astTypes.find(node->func);
        if (!callee)
            return true;

        const Luau::FunctionType* fn = Luau::get<Luau::FunctionType>(Luau::follow(*callee));
        if (!fn)
            return true;

        /*
        A platform call that a handler refines names nothing, and that is
        what luau-lsp shows for the same calls: `game:GetService("Players")`
        and `Instance.new("Part")` read whole as written, and a `className:`
        in front of every one would be the loudest word on the screen.
        A bare global still names its arguments; `require` through a string
        is the one bare call where the argument is the whole sentence.
        */
        if (fn->magic && !node->func->is<Luau::AstExprGlobal>())
            return true;

        if (auto* global = node->func->as<Luau::AstExprGlobal>();
            global && strcmp(global->name.value, "require") == 0 && node->args.size >= 1
            && node->args.data[0]->is<Luau::AstExprConstantString>())
            return true;

        size_t offset = node->self ? 1 : 0;

        for (size_t i = 0; i < node->args.size; ++i)
        {
            Luau::AstExpr* arg = node->args.data[i];
            size_t index = i + offset;

            if (!arg || index >= fn->argNames.size())
                break;

            const std::optional<Luau::FunctionArgument>& name = fn->argNames[index];
            if (!name || name->name.empty() || name->name == "self" || name->name == "_")
                continue;

            if (nameMode == 1 && !arg->is<Luau::AstExprConstantBool>() && !arg->is<Luau::AstExprConstantNumber>()
                && !arg->is<Luau::AstExprConstantString>() && !arg->is<Luau::AstExprConstantNil>())
                continue;

            if (auto* local = arg->as<Luau::AstExprLocal>(); local && sameWord(name->name, local->local->name.value))
                continue;

            if (auto* global = arg->as<Luau::AstExprGlobal>(); global && sameWord(name->name, global->name.value))
                continue;

            if (auto* indexed = arg->as<Luau::AstExprIndexName>(); indexed && sameWord(name->name, indexed->index.value))
                continue;

            found.push_back({arg->location.begin, {name->name + ":", 2}});
        }

        return true;
    }
};

size_t larvae_inlay_hints(LarvaeSession* s, const char* path, LarvaeHint* out, size_t cap,
                          int want_variables, int want_parameters, int want_returns, int name_mode)
{

    auto it = s->open.find(path);
    if (it == s->open.end())
        return 0;

    try
    {
        s->frontend.check(path);
    }
    catch (const std::exception&)
    {
        return 0;
    }

    Luau::ModulePtr module = s->frontend.moduleResolver.getModule(path);
    const Luau::SourceModule* source = s->frontend.getSourceModule(path);
    if (!module || !source || !source->root)
        return 0;

    HintCollector collector(s, module);
    collector.wantVariables = want_variables != 0;
    collector.wantParameters = want_parameters != 0;
    collector.wantReturns = want_returns != 0;
    collector.nameMode = name_mode;
    source->root->visit(&collector);

    s->hintStorage.clear();
    s->hintStorage.reserve(collector.found.size());

    for (auto& entry : collector.found)
        s->hintStorage.push_back(entry.second.first);

    size_t n = collector.found.size() < cap ? collector.found.size() : cap;

    for (size_t i = 0; i < n; ++i)
    {
        out[i].line = collector.found[i].first.line;
        out[i].character = collector.found[i].first.column;
        out[i].label = s->hintStorage[i].c_str();
        out[i].kind = collector.found[i].second.second;
    }

    return collector.found.size();
}

/*
The documentation symbol at a position, for the database Rust holds.

Luau answers this itself. The symbol names a page of the Roblox reference,
ex: `@roblox/globaltype/Player`, and it is the one handle a card has onto
prose that no type carries.
*/
const char* larvae_documentation_symbol(LarvaeSession* s, const char* path, uint32_t byte)
{
    auto it = s->open.find(path);
    if (it == s->open.end())
        return nullptr;

    Luau::ModulePtr module = strictCheck(s, path);
    const Luau::SourceModule* source = s->frontend.getSourceModule(path);
    if (!module || !source)
        return nullptr;

    LineIndex lines(it->second);
    Luau::Position position = lines.positionOf(byte);

    // Prose has no documentation of its own.
    if (Luau::isWithinComment(*source, position))
        return nullptr;

    std::optional<Luau::DocumentationSymbol> symbol;

    try
    {
        symbol = Luau::getDocumentationSymbolAtPosition(*source, *module, position);
    }
    catch (const std::exception&)
    {
        return nullptr;
    }

    /*
    A name whose own page Luau cannot find answers with its type's page.

    Luau names a page for a member and for a global, and not for a local or
    for a type reference. So `local Players = game:GetService("Players")`
    and the `Player` in `{ [Player]: ... }` had no prose at all, while
    luau-lsp shows the page of the class in both. The class is what the
    reader is looking at either way.
    */
    if (!symbol || symbol->empty())
    {
        std::optional<Luau::TypeId> type;

        if (Luau::ScopePtr scope = Luau::findScopeAtPosition(*module, position))
        {
            Luau::ExprOrLocal found = Luau::findExprOrLocalAtPosition(*source, position);

            if (Luau::AstLocal* local = found.getLocal())
                type = scope->lookup(local);

            /*
            A local on the left of an assignment records no type for the
            name it writes to. `Reliable = RemoteEvent` therefore had no
            page, while the same name two lines above did. The scope knows
            what the local is, and the card is about the class either way.
            */
            if (!type)
            {
                if (Luau::AstExpr* expr = found.getExpr())
                {
                    if (auto local = expr->as<Luau::AstExprLocal>())
                        type = scope->lookup(local->local);
                }
            }

            /*
            A type reference names its own type, and the type namespace is
            asked separately from the value namespace.
            */
            if (!type)
            {
                TypeAtPosition finder(position);
                source->root->visit(&finder);

                if (Luau::AstTypeReference* ref = finder.found)
                {
                    std::optional<Luau::TypeFun> fun = ref->prefix
                        ? scope->lookupImportedType(ref->prefix->value, ref->name.value)
                        : scope->lookupType(ref->name.value);

                    if (fun)
                        type = fun->type;
                }
            }

            /*
            The name of an alias stands for whatever the alias holds.

            `type testing2 = NumberRange` is a class under another name, and
            hovering the name it was given showed no page while hovering the
            class showed one. The card says the same thing either way.
            */
            if (!type)
            {
                NodeAtPosition at(position);
                source->root->visit(&at);

                if (at.best)
                {
                    if (auto alias = at.best->as<Luau::AstStatTypeAlias>())
                    {
                        if (std::optional<Luau::TypeFun> fun = scope->lookupType(alias->name.value))
                            type = fun->type;
                    }
                }
            }
        }

        if (!type)
            type = Luau::findTypeAtPosition(*module, *source, position);

        if (!type)
            return nullptr;

        const Luau::ExternType* etv = Luau::get<Luau::ExternType>(Luau::follow(*type));

        if (!etv)
            return nullptr;

        /*
        A sourcemap node extends its class and has no page of its own, so
        the walk climbs to the first ancestor with a real name. The class
        is what the reader is looking at: `ReplicatedStorage` typed by the
        tree still reads the service's page.
        */
        while (etv && etv->name.rfind("_larvae_", 0) == 0)
        {
            const Luau::ExternType* parent =
                etv->parent ? Luau::get<Luau::ExternType>(Luau::follow(*etv->parent)) : nullptr;

            etv = parent;
        }

        if (!etv)
            return nullptr;

        s->documentationStorage = "@roblox/globaltype/" + etv->name;

        return s->documentationStorage.c_str();
    }

    s->documentationStorage = *symbol;

    return s->documentationStorage.c_str();
}

size_t larvae_completions(LarvaeSession* s, const char* path, uint32_t byte, LarvaeCompletion* out, size_t cap)
{
    auto it = s->open.find(path);
    if (it == s->open.end())
        return 0;

    // One list reads one view of every module its entries were declared in.
    s->commentText.clear();

    LineIndex lines(it->second);
    Luau::Position position = lines.positionOf(byte);

    Luau::AutocompleteResult result;

    try
    {
        /*
        Autocomplete reads the module that the autocomplete typechecker
        built, which exists only after a check that asked for it.
        */
        Luau::FrontendOptions forAutocomplete = options();
        forAutocomplete.forAutocomplete = true;
        s->frontend.check(path, forAutocomplete);

        result = Luau::autocomplete(s->frontend, path, position,
            [](std::string, std::optional<const Luau::ExternType*>,
                std::optional<std::string>) -> std::optional<Luau::AutocompleteEntryMap> {
                return std::nullopt;
            });
    }
    catch (const std::exception&)
    {
        return 0;
    }

    /*
    After the check, because a check builds the modules whose arenas these
    are, and before the loop, which reads a type only when its arena is one
    of them.
    */
    gatherArenas(s);

    s->completionStorage.clear();
    /*
    Six strings per entry at most: the label, the type, the argument
    names, the insertion, the documentation, and the documentation symbol.
    Reserved for the same reason as the diagnostics: no reallocation after
    the first pointer is handed out. A seventh push would reallocate and
    every pointer already handed out would point at freed memory, so the
    number lives here and the loop stops rather than crossing it.
    */
    const size_t kStringsPerEntry = 6;

    s->completionStorage.reserve(cap * kStringsPerEntry);
    size_t n = 0;

    /*
    The type alone, with no argument names in it.

    The names go in the label detail, where an editor draws them against the
    label, and repeating them here would fill the row twice. This is the
    split luau-lsp makes: `(self, className)` beside the name, and
    `(Object, string) -> boolean` after it.
    */
    Luau::ToStringOptions detail;
    detail.exhaustive = false;
    detail.functionTypeArguments = false;
    detail.hideTableKind = true;
    detail.maxTypeLength = 200;

    for (const auto& [label, entry] : result.entryMap)
    {
        if (n >= cap)
            break;

        /*
        larvae's own generated names are not names a reader can write.

        The sourcemap tree and the Studio mirror each declare one extern
        type per instance, `_larvae_sourcemap_2_17` and the like, and a type
        position offers every type in scope. A real project has hundreds of
        instances, so the list filled with generated names and the project's
        own aliases fell off the end of it. The skip comes before the cap,
        so they take no room either. A card renders these as the class they
        stand for, which is the name a reader knows them by.
        */
        if (label.rfind("_larvae_", 0) == 0)
            continue;

        /*
        The room this entry needs, before any of it is handed out. The
        reserve above covers every entry the cap allows, so this only ever
        fires if somebody adds a seventh string and forgets the number.
        */
        if (s->completionStorage.size() + kStringsPerEntry > s->completionStorage.capacity())
            break;

        s->completionStorage.push_back(label);
        out[n].label = s->completionStorage.back().c_str();

        uint8_t kind = 12; /* Value */
        switch (entry.kind)
        {
        case Luau::AutocompleteEntryKind::Property:
            kind = 5; /* Field */
            break;
        case Luau::AutocompleteEntryKind::Keyword:
            kind = 14; /* Keyword */
            break;
        case Luau::AutocompleteEntryKind::Module:
            kind = 9; /* Module */
            break;
        case Luau::AutocompleteEntryKind::Type:
            kind = 8; /* Interface */
            break;
        default:
            break;
        }

        /*
        A function reads as a function, unless the entry is a type. `type
        Handler = () -> ()` is a type the author writes in an annotation,
        and an editor that drew it as a callable offered a call where no
        call belongs. luau-lsp draws the same line.
        */
        if (entry.kind != Luau::AutocompleteEntryKind::Type && entry.type
            && Luau::get<Luau::FunctionType>(Luau::follow(*entry.type)))
            kind = 3; /* Function */

        out[n].kind = kind;
        out[n].deprecated = entry.deprecated ? 1 : 0;
        out[n].wrong_index_type = entry.wrongIndexType ? 1 : 0;

        switch (entry.typeCorrect)
        {
        case Luau::TypeCorrectKind::Correct:
            out[n].type_correct = 1;
            break;
        case Luau::TypeCorrectKind::CorrectFunctionResult:
            out[n].type_correct = 2;
            break;
        default:
            out[n].type_correct = 0;
            break;
        }

        out[n].detail = nullptr;
        out[n].label_detail = nullptr;
        out[n].insert_text = nullptr;
        out[n].documentation = nullptr;
        out[n].documentation_symbol = nullptr;

        /*
        Luau asks for the parentheses itself. `parens` says whether the
        entry is called and whether it takes arguments, so an editor writes
        `IsA()` with the caret inside and `GetChildren()` with it after.
        */
        if (entry.parens != Luau::ParenthesesRecommendation::None)
        {
            s->completionStorage.push_back(label + "()");
            out[n].insert_text = s->completionStorage.back().c_str();
        }
        else if (entry.insertText && !entry.insertText->empty())
        {
            s->completionStorage.push_back(*entry.insertText);
            out[n].insert_text = s->completionStorage.back().c_str();
        }

        if (entry.documentationSymbol && !entry.documentationSymbol->empty())
        {
            s->completionStorage.push_back(*entry.documentationSymbol);
            out[n].documentation_symbol = s->completionStorage.back().c_str();
        }

        if (entry.type)
        {
            std::string rendered;

            try
            {
                rendered = Luau::toString(Luau::follow(*entry.type), detail);
            }
            catch (const std::exception&)
            {
                rendered.clear();
            }

            if (!rendered.empty())
            {
                s->completionStorage.push_back(rendered);
                out[n].detail = s->completionStorage.back().c_str();
            }

            /*
            The names of the arguments, in parentheses.

            A reader picking from a list wants to know what a call takes,
            and the type alone says `(Object, string)` where the source
            says `(self, className)`. luau-lsp draws the names here and the
            types in the detail, so the row carries both.
            */
            if (const Luau::FunctionType* fn
                = Luau::get<Luau::FunctionType>(Luau::follow(*entry.type)))
            {
                std::string names = "(";
                size_t written = 0;

                // An argument the declaration did not name reads as `_`.
                for (const std::optional<Luau::FunctionArgument>& argument : fn->argNames)
                {
                    if (written > 0)
                        names += ", ";

                    names += (argument && !argument->name.empty()) ? argument->name : "_";
                    ++written;
                }

                names += ")";

                if (written > 0)
                {
                    s->completionStorage.push_back(names);
                    out[n].label_detail = s->completionStorage.back().c_str();
                }
            }

            std::string docs = documentationOfType(s, *entry.type);

            if (!docs.empty())
            {
                s->completionStorage.push_back(docs);
                out[n].documentation = s->completionStorage.back().c_str();
            }
        }

        ++n;
    }

    return n;
}

/*
One compile error, in the line luau-lsp writes for it.

The kind, the one based line and column, then the message. luau-lsp shows
this text in the same panel the listing goes to, so a file that does not
compile still says why rather than opening empty.
*/
static std::string compileFailure(const char* kind, const Luau::Location& at, const char* message)
{
    return std::string(kind) + "(" + std::to_string(at.begin.line + 1) + ","
        + std::to_string(at.begin.column + 1) + "): " + message + "\n";
}

/*
The compiled form of one source text.

The dump flags are the four luau-lsp asks for. `Dump_Types` prints the
`R0: number [argument]` lines that `typeInfoLevel` exists to produce;
`Dump_Locals` prints the raw debug table instead, which luau-lsp does not
show, so it stays off. `Dump_Constants` is off for the same reason.

The vector strings decide whether the compiler folds a project's own vector
constructor. An empty one means Luau's default, so a project that says
nothing keeps `vector.create` and nothing else.
*/
const char* larvae_bytecode(LarvaeSession* s, const char* source, int optimization, int remarks,
                            int debug_level, int type_info_level, const char* vector_lib,
                            const char* vector_ctor, const char* vector_type)
{
    const std::string text(source ? source : "");

    /*
    The options hold pointers, so the strings live until the compile ends.
    */
    const std::string lib(vector_lib ? vector_lib : "");
    const std::string ctor(vector_ctor ? vector_ctor : "");
    const std::string type(vector_type ? vector_type : "");

    Luau::CompileOptions options;
    options.optimizationLevel = optimization;
    options.debugLevel = debug_level;
    options.typeInfoLevel = type_info_level;
    options.vectorLib = lib.empty() ? nullptr : lib.c_str();
    options.vectorCtor = ctor.empty() ? nullptr : ctor.c_str();
    options.vectorType = type.empty() ? nullptr : type.c_str();

    Luau::BytecodeBuilder builder;

    builder.setDumpFlags(
        Luau::BytecodeBuilder::Dump_Code | Luau::BytecodeBuilder::Dump_Source
        | Luau::BytecodeBuilder::Dump_Types | Luau::BytecodeBuilder::Dump_Remarks);
    builder.setDumpSource(text);

    try
    {
        Luau::compileOrThrow(builder, text, options);
    }
    catch (Luau::ParseErrors& errors)
    {
        s->bytecodeStorage.clear();

        for (const Luau::ParseError& error : errors.getErrors())
            s->bytecodeStorage += compileFailure("SyntaxError", error.getLocation(), error.what());

        return s->bytecodeStorage.c_str();
    }
    catch (Luau::CompileError& error)
    {
        s->bytecodeStorage = compileFailure("CompileError", error.getLocation(), error.what());

        return s->bytecodeStorage.c_str();
    }
    catch (const std::exception& error)
    {
        s->bytecodeStorage = std::string(error.what()) + "\n";

        return s->bytecodeStorage.c_str();
    }

    /*
    The remarks view is the source with the compiler's decisions written
    above the lines they belong to, which is what luau-lsp serves under
    `compilerRemarks`. The other view is the listing.
    */
    s->bytecodeStorage = remarks ? builder.dumpSourceRemarks() : builder.dumpEverything();

    return s->bytecodeStorage.c_str();
}

} // extern "C"
