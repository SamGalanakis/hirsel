# Clone hooks in two files of one module

U and V each have a clone hook at item ordinal 1 of a different source file in
core.clonehooks. U's hook adds 100 to 1; V's adds 200 to 2. Therefore cloning both
returns 101 * 1000 + 202 = 101202. Both values are dropped, retaining 0 bytes.
Neither declaration may acquire the other file's hook identity.
