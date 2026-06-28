using System;
using UnityEngine;

// A scratch file for trying the Unterm code editor's C# completion. Open it in the
// Unterm editor (right-click ▸ Open in Unterm Code Editor, or set Unterm as the
// External Script Editor). Each method below notes what to type and what should pop.
// Many spots now pop WITHOUT typing a letter (after `(` `,` `new ` `case ` `= `, etc.).
// It compiles as-is; the completion hints live in comments, so nothing here is wrong.
public class CompletionPlayground : MonoBehaviour
{
    public enum Phase { Idle, Charging, Firing, Cooldown }

    public int Health;
    public string Label;
    public Phase Current;

    // 1) MEMBER — type `transform.` then a letter → members of Transform.
    void Member()
    {
        // transform.
        // Health.
    }

    // 2) GENERAL (scope) — type a letter → locals, fields, types, keywords. Each row
    //    shows a kind badge (N=namespace, T=type, M=method, P=property, F=field, …);
    //    types/members rank ABOVE namespaces (UnityEngine sinks down).
    void General()
    {
        int speed = 3;
        // sp   → `speed`
        // Game → `GameObject`, …  (types first, then namespaces)
    }

    // 3) ATTRIBUTE — type `[` above a field → only Attribute types (no letter needed).
    // [
    [SerializeField] int _hidden;

    // 4) NEW — type `new ` (then nothing) → types only. `new Vec` → `Vector3`, …
    void New()
    {
        // var go = new
        // var v  = new Vec
    }

    // 5) NAMED ARGUMENTS — right after `(` or `,` (no letter) → parameter names `name:`
    //    first, alongside scope symbols.
    void NamedArgs()
    {
        // transform.Translate(   → `translation:`, `relativeTo:`
        // Mathf.Clamp(           → `value:`, `min:`, `max:`
    }

    // 6) ENUM value — where an enum is expected, its qualified members lead (pop on the
    //    space after `=` / `case`, no letter needed):
    void EnumValue()
    {
        // Phase p =        → `Phase.Idle`, `Phase.Charging`, …
        // if (Current ==   → `Phase.Charging`, …
        switch (Current)
        {
            // case   → `Phase.Idle`, `Phase.Firing`, …  (qualified)
            default: break;
        }
    }

    // 7) OBJECT INITIALIZER — inside `new T { … }` → settable members as `Name = `.
    void Initializer()
    {
        // var r = new Rect {   → `x = `, `y = `, `width = `, `height = `
    }

    // 8) USING — on a `using ` line, type a letter → namespaces only.
    //    (at the top of the file:  using Sys  → `System`)

    // 9) UNIMPORTED TYPE + AUTO-USING — type a type whose namespace ISN'T imported here
    //    (only System + UnityEngine are). After ~3 letters it appears as
    //    `Name  (Namespace)`; accepting it inserts the `using` at the top.
    void AutoUsing()
    {
        // List       → `List  (System.Collections.Generic)`  → adds `using System.Collections.Generic;`
        // StringBui  → `StringBuilder  (System.Text)`        → adds `using System.Text;`
        // Stopwatch  → `Stopwatch  (System.Diagnostics)`
    }
}

// 10) OVERRIDE — in `Pup` below, type `override ` (then nothing) → the base's virtual
//     members appear with a generated signature; accepting replaces `override` with the
//     full `public override … { base.… }`. (Also works for object's ToString/Equals.)
public class Critter
{
    public virtual void Speak() { }
    public virtual int Legs => 4;
    public virtual string Describe(int mood) => "";
}

public class Pup : Critter
{
    // override   → `Speak()`, `Legs`, `Describe(int mood)`, `ToString()`, …
}
