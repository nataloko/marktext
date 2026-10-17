# Stand-ins for Electron main process modules

The ☰ menu reuses the Electron menu templates and most menu actions from
`packages/desktop/src/main/menu` unchanged. Some modules they import use Node,
Electron runtime APIs or `process` while loading, which a web page cannot run.

`vite.config.ts` swaps those modules: any import that resolves to
`packages/desktop/src/<path>` loads `src/stand-ins/<path>` instead when that file
exists. Each stand-in keeps the original's export names, and the build fails if
a reused module imports a name a stand-in lacks.

Type checking still sees the originals, so the reused modules are checked
against the real Electron types.

When upstream changes one of the originals, check whether its stand-in needs
the same change.
