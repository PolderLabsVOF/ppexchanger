# ppexchanger

Install the ppexchanger terminal messenger with npm:

```sh
npm install --global ppexchanger
ppx
```

The package downloads the matching ppx release for Linux, macOS, or Windows.
The executable is still called `ppx`.
No npm lifecycle scripts need to be enabled; the native binary is downloaded
the first time you run `ppx`.

For a project-local install, run it with npm so an older global `ppx` does not
get selected accidentally:

```sh
npm install ppexchanger
npx --no-install ppexchanger
```
It does not create an account or use a server. For more information, visit
the [ppexchanger project](https://github.com/PolderLabsVOF/ppexchanger).
