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
        if (!resolve || !context)
            return std::nullopt;

        auto* expr = node->as<Luau::AstExprConstantString>();
        if (!expr)
            return std::nullopt;

        std::string spec(expr->value.data, expr->value.size);
        const char* target = resolve(userdata, context->name.c_str(), spec.c_str());
        if (!target)
            return std::nullopt;

        return Luau::ModuleInfo{target};
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

static void enableAllFlags()
{
    // Snapshot before the first change, or there is nothing to put back.
    savedFlags();

    for (Luau::FValue<bool>* it = Luau::FValue<bool>::list; it; it = it->next)
    {
        if (strncmp(it->name, "Luau", 4) == 0 && !Luau::isAnalysisFlagExperimental(it->name))
            it->value = true;
    }
}

struct LarvaeSession
{
    RustFileResolver files;
    Luau::NullConfigResolver configs;
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

    /* The declared type of `script`, per module. See larvae_set_script_type. */
    std::map<std::string, std::string> scriptTypes;

    LarvaeSession()
        : frontend(&files, &configs, options())
    {
        files.open = &open;
        configs.defaultConfig.mode = Luau::Mode::Nonstrict;
        applyRequiredFlags();

        Luau::registerBuiltinGlobals(frontend, frontend.globals, false);
        Luau::registerBuiltinGlobals(frontend, frontend.globalsForAutocomplete, true);
        Luau::freeze(frontend.globals.globalTypes);
        Luau::freeze(frontend.globalsForAutocomplete.globalTypes);

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
    for (auto& entry : savedFlags())
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
struct MagicServiceLookup final : Luau::MagicFunction
{
    static std::optional<Luau::TypeId> named(Luau::Scope* scope, const Luau::AstExprCall& call)
    {
        if (call.args.size < 1)
            return std::nullopt;

        auto text = call.args.data[0]->as<Luau::AstExprConstantString>();
        if (!text || !scope)
            return std::nullopt;

        std::string name(text->value.data, text->value.size);
        std::optional<Luau::TypeFun> found = scope->lookupType(name);

        // A generic type is not a service, and substituting one needs arguments.
        if (!found || !found->typeParams.empty() || !found->typePackParams.empty())
            return std::nullopt;

        return Luau::follow(found->type);
    }

    std::optional<Luau::WithPredicate<Luau::TypePackId>> handleOldSolver(
        Luau::TypeChecker& typeChecker,
        const Luau::ScopePtr&,
        const Luau::AstExprCall& call,
        Luau::WithPredicate<Luau::TypePackId>) override
    {
        std::optional<Luau::TypeId> service = named(typeChecker.globalScope.get(), call);
        if (!service)
            return std::nullopt;

        Luau::TypeArena& arena = *typeChecker.currentModule->internalTypes;

        return Luau::WithPredicate<Luau::TypePackId>{arena.addTypePack({*service})};
    }

    bool infer(const Luau::MagicFunctionCallContext& context) override
    {
        std::optional<Luau::TypeId> service = named(context.solver->rootScope.get(), *context.callSite);
        if (!service)
            return false;

        Luau::TypePackId pack = context.solver->arena->addTypePack({*service});
        asMutable(context.result)->ty.emplace<Luau::BoundTypePack>(pack);

        return true;
    }
};

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
    Luau::unfreeze(s->frontend.globalsForAutocomplete.globalTypes);

    Luau::LoadDefinitionFileResult result = s->frontend.loadDefinitionFile(
        s->frontend.globals, s->frontend.globals.globalScope, source, name, false, false);

    Luau::LoadDefinitionFileResult forAutocomplete = s->frontend.loadDefinitionFile(
        s->frontend.globalsForAutocomplete, s->frontend.globalsForAutocomplete.globalScope, source, name, false, true);

    /*
    Both tables get the handler, and the autocomplete one matters most: a
    hover reads that table, so a handler only on the other one would leave
    the card saying `Instance` while a completion knew better.
    */
    attachServiceLookup(s->frontend.globals);
    attachServiceLookup(s->frontend.globalsForAutocomplete);

    Luau::freeze(s->frontend.globals.globalTypes);
    Luau::freeze(s->frontend.globalsForAutocomplete.globalTypes);

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

    return result.success && forAutocomplete.success ? 0 : 1;
}

void larvae_open(LarvaeSession* s, const char* path, const char* text)
{
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
static Luau::ModulePtr strictCheck(LarvaeSession* s, const char* path)
{
    Luau::ModulePtr had = s->frontend.moduleResolverForAutocomplete.getModule(path);

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
    catch (const std::exception&)
    {
        return nullptr;
    }

    return s->frontend.moduleResolverForAutocomplete.getModule(path);
}

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

    Luau::ModulePtr module = strictCheck(s, path);
    const Luau::SourceModule* source = s->frontend.getSourceModule(path);
    if (!module || !source)
        return nullptr;

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
    A type name answers with what it stands for.

    `type Point = { x: number }` and every later `Point` both read as the
    alias, so hovering either shows the shape the name hides. The type
    namespace is separate from the value namespace, so the scope is asked a
    different question here.
    */
    /*
    Types are asked for outright. The walk leaves them out by default, so a
    `Point` in `local p: Point` was never reached and the reference hovered
    as nothing while its declaration hovered fine.
    */
    std::vector<Luau::AstNode*> ancestry =
        Luau::findAstAncestryOfPosition(*source, position, /* includeTypes = */ true);

    if (scope && !ancestry.empty())
    {
        /*
        The innermost type reference the position sits in, and not only the
        last node.

        A `number` inside a parameter list has the function above it in the
        walk, and taking the last node alone found the function. So hovering
        a type annotation showed the signature of whatever held it, which is
        never what the cursor is on.
        */
        TypeAtPosition finder(position);
        source->root->visit(&finder);

        Luau::AstTypeReference* ref = finder.found;

        if (ref)
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
        else
        {
            // The declaration itself, which is one node above the name.
            for (auto up = ancestry.rbegin(); up != ancestry.rend(); ++up)
            {
                auto alias = (*up)->as<Luau::AstStatTypeAlias>();

                if (!alias || !alias->nameLocation.containsClosed(position))
                    continue;

                if (auto fun = scope->lookupType(alias->name.value))
                {
                    aliasName = alias->name.value;
                    aliasParameters = *fun;
                    type = fun->type;
                }

                break;
            }
        }
    }

    // A local is not an expression, so the scope answers for it.
    if (!type)
    if (Luau::AstLocal* local = found.getLocal())
    {
        if (scope)
            type = scope->lookup(local);
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

    if (!type)
        type = Luau::findTypeAtPosition(*module, *source, position);

    /*
    A field or a method reached through a dot.

    `findTypeAtPosition` answers for the expression that starts at the
    position, and the name in `a.b` starts after the dot, so `b` answered
    with nothing. The type of the whole index expression is what a reader
    hovering `b` is asking about, and the module recorded it.
    */
    // Kept for the card's name: the index expression the position sits in.
    Luau::AstExprIndexName* hovered_index = nullptr;

    /*
    The type a call's callee was declared with, before the call solved it.

    `table.create<V>(count, value)` reads as `table.create(count: number,
    value: nil)` at a call site, because the recorded type of the expression
    is the instantiated one. A reader hovering the name wants the signature
    they can call, generics and all, and that is what the frontend keeps
    under the call. luau-lsp reads the same map.
    */
    for (auto up = ancestry.rbegin(); up != ancestry.rend(); ++up)
    {
        auto call = (*up)->as<Luau::AstExprCall>();

        if (!call || !call->func)
            continue;

        /*
        Only the name that is called, and not the receiver in front of it.
        Hovering `world` in `world:add()` asks about `world`, and answering
        with the signature of `add` is the wrong question answered.
        */
        bool on_the_name = false;

        if (auto index = call->func->as<Luau::AstExprIndexName>())
            on_the_name = index->indexLocation.containsClosed(position);
        else if (call->func->is<Luau::AstExprGlobal>() || call->func->is<Luau::AstExprLocal>())
            on_the_name = call->func->location.containsClosed(position);

        if (!on_the_name)
            break;

        if (auto original = module->astOriginalCallTypes.find(call->func))
            type = *original;

        break;
    }

    for (auto up = ancestry.rbegin(); up != ancestry.rend(); ++up)
    {
        auto index = (*up)->as<Luau::AstExprIndexName>();

        if (!index || !index->location.containsClosed(position))
            continue;

        hovered_index = index;

        if (!type)
        {
            if (auto found = module->astTypes.find(index))
                type = *found;
        }

        break;
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
    A function shows its signature. The name comes from whichever half of
    the answer carries one, and a function with no name still renders, so an
    anonymous one is not left blank.
    */
    if (const Luau::FunctionType* ftv = Luau::get<Luau::FunctionType>(followed))
    {
        std::string name;
        // The expression the method hangs off, so the card names its type.
        Luau::AstExpr* receiver = nullptr;

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
        else if (Luau::AstLocal* local = found.getLocal())
            name = local->name.value;

        if (expr && name.empty())
        {
            if (auto global = expr->as<Luau::AstExprGlobal>())
                name = global->name.value;
            else if (auto localExpr = expr->as<Luau::AstExprLocal>())
                name = localExpr->local->name.value;
            else if (auto index = expr->as<Luau::AstExprIndexName>())
            {
                receiver = index->expr;

                /*
                The whole path, so a card reads `math.cos` and not `cos`.

                A reader hovering a name in `math.cos` wants to know where it
                came from, and the bare name says nothing they did not
                already see. The separator is the one the author wrote, so a
                method keeps its colon.
                */
                std::string path(index->index.value);

                /*
                A method is named for the type it hangs off and not for the
                variable that holds one. `p:Destroy()` reads as
                `Instance:Destroy()`, because the card is about the method
                and every `Instance` has it. A field keeps the path the
                author wrote, so `math.cos` stays `math.cos`.
                */
                if (index->op == ':')
                {
                    name = path;

                    goto named;
                }

                // A field keeps the path the author wrote, and names no type.
                receiver = nullptr;

                for (Luau::AstExpr* walk = index->expr; walk;)
                {
                    if (auto step = walk->as<Luau::AstExprIndexName>())
                    {
                        path = std::string(step->index.value) + std::string(1, index->op) + path;
                        walk = step->expr;

                        continue;
                    }

                    if (auto global = walk->as<Luau::AstExprGlobal>())
                        path = std::string(global->name.value) + std::string(1, index->op) + path;
                    else if (auto local = walk->as<Luau::AstExprLocal>())
                        path = std::string(local->local->name.value) + std::string(1, index->op) + path;

                    break;
                }

                /*
                Two segments are enough. `PlanckRunService.Plugin.new` says
                no more than `Plugin.new` about where the function came
                from, and the card is one line. luau-lsp keeps two.
                */
                size_t cut = path.rfind('.');

                if (cut != std::string::npos && cut > 0)
                {
                    size_t before = path.find_last_of(".:", cut - 1);

                    if (before != std::string::npos)
                        path = path.substr(before + 1);
                }

                name = path;
            }
        }

    named:

        /*
        A method reads as `Instance:Destroy()`, not `Destroy(self: Instance)`.

        The receiver is the first argument in the type and the author never
        wrote it, so showing it puts a parameter in the card that does not
        exist in the source. Luau hides it on request, and the name carries
        the type it hangs off instead, which is how luau-lsp writes one and
        how the Roblox documentation writes one.
        */
        if (ftv->hasSelf)
            opts.hideFunctionSelfArgument = true;

        /*
        The type the method hangs off goes in front of its name.

        `p:Destroy()` reads as `Instance:Destroy()`, because the card is
        about the method and every `Instance` has it. The receiver of the
        call names it, and not the `self` of the declaration:
        `game:GetService` read as `ServiceProvider:GetService`, which is
        where the method is declared and not what the reader wrote.

        A method whose type does not carry `self` still gets the prefix. The
        author wrote a colon, so the card should show one, and a table that
        holds its methods without a `self` argument is the common shape of a
        Luau module.
        */
        if (receiver && name.find(':') == std::string::npos
            && name.find('.') == std::string::npos)
        {
            Luau::ToStringOptions bare;
            bare.exhaustive = false;

            std::optional<Luau::TypeId> base_type;

            if (Luau::TypeId* found_receiver = module->astTypes.find(receiver))
                base_type = Luau::follow(*found_receiver);

            if (!base_type && ftv->hasSelf)
            {
                auto [args, tail] = Luau::flatten(ftv->argTypes);
                (void)tail;

                if (!args.empty())
                    base_type = Luau::follow(args[0]);
            }

            if (base_type)
            {
                std::string base = Luau::toString(*base_type, bare);

                // A base that renders as a whole table is noise, not a name.
                if (!base.empty() && base.find('{') == std::string::npos
                    && base.find('(') == std::string::npos)
                {
                    name = base + ":" + name;
                }
            }
        }

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
        signature.hideFunctionSelfArgument = opts.hideFunctionSelfArgument;
        signature.hideTableKind = opts.hideTableKind;
        signature.scope = scope;

        /*
        The `function` keyword goes in front, because the card should read
        like the line the author would write. Luau renders the rest.
        */
        s->hoverStorage = name.empty()
            ? "function" + Luau::toStringNamedFunction("", *ftv, signature)
            : "function " + Luau::toStringNamedFunction(name, *ftv, signature);

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

    // A type name says it is a type, and what it stands for.
    if (!aliasName.empty())
    {
        /*
        The parameters of the alias come with the name.

        `type Entity = { __T: T }` says nothing about where `T` comes from,
        and the alias is generic: `type Entity<T = nil>` is the line the
        author wrote and the one a reader is looking for.
        */
        std::string parameters;

        if (aliasParameters && !aliasParameters->typeParams.empty())
        {
            Luau::ToStringOptions bare;
            bare.exhaustive = false;

            parameters = "<";

            for (size_t i = 0; i < aliasParameters->typeParams.size(); ++i)
            {
                if (i > 0)
                    parameters += ", ";

                const Luau::GenericTypeDefinition& param = aliasParameters->typeParams[i];

                parameters += Luau::toString(param.ty, bare);

                if (param.defaultValue)
                    parameters += " = " + Luau::toString(*param.defaultValue, bare);
            }

            parameters += ">";
        }

        s->hoverStorage = "type " + aliasName + parameters + " = " + text;

        return s->hoverStorage.c_str();
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

    if (tail)
        s->parameterStorage.push_back("...");

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

Two kinds, and both answer the same question: what is the type the author
did not write. A local with no annotation gets its inferred type after the
name. A function parameter and a return type get the same, which is what
makes an unannotated codebase readable without changing it.

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

    Luau::ToStringOptions opts;

    HintCollector(LarvaeSession* s, Luau::ModulePtr m)
        : session(s)
        , module(std::move(m))
    {
        opts.exhaustive = false;
        opts.maxTypeLength = 60;
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
        for (size_t i = 0; i < node->vars.size; ++i)
        {
            Luau::AstLocal* local = node->vars.data[i];

            // An annotation puts the type on screen already.
            if (!local || local->annotation)
                continue;

            if (i >= node->values.size)
                break;

            auto* type = module->astTypes.find(node->values.data[i]);
            if (!type)
                continue;

            if (auto text = render(*type))
                found.push_back({local->location.end, {": " + *text, 1}});
        }

        return true;
    }

    bool visit(Luau::AstExprFunction* node) override
    {
        auto* self = module->astTypes.find(node);
        if (!self)
            return true;

        const Luau::FunctionType* fn = Luau::get<Luau::FunctionType>(Luau::follow(*self));
        if (!fn)
            return true;

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

        return true;
    }
};

size_t larvae_inlay_hints(LarvaeSession* s, const char* path, LarvaeHint* out, size_t cap)
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
Where a type was declared, when the type says so.

A function carries the module and the location it was written at, and that
is the one handle a completion entry gives onto the source a reader wants to
read. A type that carries none answers nothing, and the entry then shows its
type alone.
*/
static std::optional<std::pair<Luau::ModuleName, Luau::Location>> declaredAt(Luau::TypeId ty)
{
    ty = Luau::follow(ty);

    if (const Luau::FunctionType* fn = Luau::get<Luau::FunctionType>(ty))
    {
        // A function declared in a definitions file names no module of its own.
        if (fn->definition && fn->definition->definitionModuleName)
            return std::make_pair(*fn->definition->definitionModuleName, fn->definition->definitionLocation);
    }

    return std::nullopt;
}

/*
The comment block that stands above one line, as the reader wrote it.

A doc comment in Luau is `--` lines or one `--[[ ]]` block, directly above
the declaration and with no blank line between. The markers come off and the
text goes through as markdown, which is what luau-lsp does and what every
editor renders.

The walk is upward from the declaration and stops at the first line that is
not a comment. So a comment that belongs to the statement above does not
travel down onto this one.
*/
static std::string commentAbove(const std::string& text, uint32_t line)
{
    std::vector<std::string> source;
    std::string current;

    for (char c : text)
    {
        if (c == '\n')
        {
            source.push_back(current);
            current.clear();
        }
        else if (c != '\r')
        {
            current += c;
        }
    }

    source.push_back(current);

    if (line >= source.size())
        return {};

    auto trim = [](const std::string& in) -> std::string
    {
        size_t start = in.find_first_not_of(" \t");
        if (start == std::string::npos)
            return {};

        size_t end = in.find_last_not_of(" \t");
        return in.substr(start, end - start + 1);
    };

    std::vector<std::string> block;
    size_t at = line;

    while (at > 0)
    {
        std::string above = trim(source[at - 1]);

        if (above.rfind("--", 0) != 0)
            break;

        /*
        A block comment is read whole, from its opening line down to the
        line above the declaration. Anything above the opening belongs to
        something else.
        */
        if (above.rfind("--[[", 0) == 0 || above.rfind("--[=[", 0) == 0)
        {
            std::vector<std::string> whole;

            for (size_t i = at - 1; i < line; ++i)
                whole.push_back(trim(source[i]));

            block.insert(block.begin(), whole.begin(), whole.end());
            break;
        }

        block.insert(block.begin(), above);
        --at;
    }

    std::string out;

    for (std::string& raw : block)
    {
        std::string kept = raw;

        for (const char* marker : {"--[=[", "--[[", "---", "--"})
        {
            if (kept.rfind(marker, 0) == 0)
            {
                kept = kept.substr(strlen(marker));
                break;
            }
        }

        // The closing of a block comment is a marker and not prose.
        for (const char* close : {"]=]", "]]"})
        {
            size_t end = kept.find(close);
            if (end != std::string::npos)
                kept = kept.substr(0, end);
        }

        kept = trim(kept);

        if (kept.empty() && out.empty())
            continue;

        out += kept;
        out += "\n";
    }

    while (!out.empty() && (out.back() == '\n' || out.back() == ' '))
        out.pop_back();

    return out;
}

/// The documentation of one entry, or empty when the session cannot read any
static std::string documentationOf(LarvaeSession* s, const Luau::AutocompleteEntry& entry)
{
    if (!entry.type)
        return {};

    std::optional<std::pair<Luau::ModuleName, Luau::Location>> where = declaredAt(*entry.type);

    if (!where)
        return {};

    std::optional<Luau::SourceCode> source = s->files.readSource(where->first);

    if (!source)
        return {};

    return commentAbove(source->source, where->second.begin.line);
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
        }

        if (!type)
            type = Luau::findTypeAtPosition(*module, *source, position);

        if (!type)
            return nullptr;

        const Luau::ExternType* etv = Luau::get<Luau::ExternType>(Luau::follow(*type));

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

    s->completionStorage.clear();
    /*
    Six strings per entry at most: the label, the type, the argument
    names, the insertion, the documentation, and the documentation symbol.
    Reserved for the same reason as the diagnostics: no reallocation after
    the first pointer is handed out.
    */
    s->completionStorage.reserve(cap * 6);
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
        default:
            break;
        }

        if (entry.type && Luau::get<Luau::FunctionType>(Luau::follow(*entry.type)))
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

            std::string docs = documentationOf(s, entry);

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

} // extern "C"
