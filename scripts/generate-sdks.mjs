import { cp, mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const root = resolve(".");
const version = (process.argv[2] ?? "0.1.0").replace(/^v/, "");
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(version)) {
  throw new Error(`invalid SDK version: ${version}`);
}

const image = "openapitools/openapi-generator-cli:v7.15.0";
const spec = "/local/openapi/openapi.yaml";
const runGenerator = (generator, output, properties) => {
  const result = spawnSync(
    "docker",
    [
      "run",
      "--rm",
      "-v",
      `${root}:/local`,
      image,
      "generate",
      "-i",
      spec,
      "-g",
      generator,
      "-o",
      `/local/${output}`,
      "--additional-properties",
      properties,
    ],
    { stdio: "inherit" },
  );
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${generator} SDK generation failed`);
};

await mkdir(resolve(root, "sdk/typescript"), { recursive: true });
await mkdir(resolve(root, "sdk/rust"), { recursive: true });
const temporaryRoot = resolve(root, "sdk/.generation");
const temporaryTypeScript = resolve(temporaryRoot, "typescript");
const temporaryRust = resolve(temporaryRoot, "rust");
await rm(temporaryRoot, { recursive: true, force: true });
await mkdir(temporaryRoot, { recursive: true });

try {
  runGenerator(
    "typescript-fetch",
    "sdk/.generation/typescript",
    `npmName=zenith-backend-sdk,npmVersion=${version},supportsES6=true`,
  );
  runGenerator(
    "rust",
    "sdk/.generation/rust",
    `packageName=zenith_backend_client,packageVersion=${version},library=reqwest`,
  );

  const generatedIndex = resolve(temporaryTypeScript, "src/index.ts");
  const index = await readFile(generatedIndex, "utf8");
  await writeFile(generatedIndex, `${index}\nexport * from "./zenith";\n`);
  await cp(
    resolve(root, "sdk/typescript/src/zenith.ts"),
    resolve(temporaryTypeScript, "src/zenith.ts"),
  );

  const packagePath = resolve(temporaryTypeScript, "package.json");
  const packageJson = JSON.parse(await readFile(packagePath, "utf8"));
  packageJson.author = "Zenith Protocol Contributors";
  packageJson.description = "Typed TypeScript client for the Zenith Backend API";
  packageJson.repository = {
    type: "git",
    url: "https://github.com/ngoziobidigwe212/backendd.git",
  };
  await writeFile(packagePath, `${JSON.stringify(packageJson, null, 2)}\n`);

  const npmIgnorePath = resolve(temporaryTypeScript, ".npmignore");
  const npmIgnore = await readFile(npmIgnorePath, "utf8");
  await writeFile(
    npmIgnorePath,
    `${npmIgnore.split(/\r?\n/).filter((entry) => entry !== "dist").join("\n")}\n`,
  );

  const cargoTomlPath = resolve(temporaryRust, "Cargo.toml");
  const cargoToml = (await readFile(cargoTomlPath, "utf8"))
    .replace('authors = ["OpenAPI Generator team and contributors"]', 'authors = ["Zenith Protocol Contributors"]')
    .replace('license = "MIT"\n', 'license = "MIT"\nrepository = "https://github.com/ngoziobidigwe212/backendd"\n');
  await writeFile(cargoTomlPath, cargoToml);

  await rm(resolve(root, "sdk/typescript/generated"), { recursive: true, force: true });
  await rm(resolve(root, "sdk/rust/generated"), { recursive: true, force: true });
  await rename(temporaryTypeScript, resolve(root, "sdk/typescript/generated"));
  await rename(temporaryRust, resolve(root, "sdk/rust/generated"));
} finally {
  await rm(temporaryRoot, { recursive: true, force: true });
}
