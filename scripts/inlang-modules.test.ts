// @vitest-environment node
//
// The interface strings are compiled by plugins the inlang SDK loads from
// project.inlang/settings.json, inside every build, the release build
// included. A module given as a URL is fetched from there on each compile
// -- no pin, no integrity check -- and then run, and `paraglide-js init`
// adds such a URL by itself. So each module has to be a file in
// node_modules from a package that package.json pins to one exact version,
// which the lockfile then holds to a hash.
import { existsSync, readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const repo = new URL("../", import.meta.url);
const json = (path: string) => JSON.parse(readFileSync(new URL(path, repo), "utf8"));

describe("inlang modules", () => {
  it("come from node_modules, pinned exactly and locked by hash", () => {
    const settings = json("project.inlang/settings.json");
    const pkg = json("package.json");
    const lock = json("package-lock.json");
    const modules: unknown[] = settings.modules ?? [];
    expect(modules.length).toBeGreaterThan(0);

    for (const module of modules) {
      // Relative to the folder that holds project.inlang: that is how the SDK
      // resolves a module path.
      expect(typeof module).toBe("string");
      const path = module as string;
      expect(path, path).not.toMatch(/(^|\/)\.\.(\/|$)/);
      const match = /^\.\/node_modules\/((?:@[^/]+\/)?[^/]+)\/.+\.js$/.exec(path);
      expect(match, `${path} is not a file under ./node_modules/`).not.toBeNull();
      const name = match![1];

      const pinned = pkg.devDependencies?.[name];
      expect(pinned, `${name} pinned in devDependencies`).toMatch(/^\d+\.\d+\.\d+$/);
      const locked = lock.packages?.[`node_modules/${name}`];
      expect(locked?.version, `${name} in package-lock.json`).toBe(pinned);
      expect(locked?.integrity, `${name} integrity`).toMatch(/^sha512-/);
      expect(existsSync(new URL(path.slice(2), repo)), `${path} installed`).toBe(true);
    }
  });
});
