import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { languages, messages, translate } from "./locales.js";

test("all application labels are nonempty in English, Spanish and French", () => {
  assert.deepEqual(languages, ["en", "es", "fr"]);
  for (const language of languages) {
    assert.deepEqual(Object.keys(messages[language]), Object.keys(messages.en));
    for (const key of Object.keys(messages.en)) assert.ok(translate(language, key).trim(), `${language}.${key}`);
  }
});

test("literal frontend label references exist and unknown labels fail explicitly", async () => {
  const source = await readFile(new URL("./main.js", import.meta.url), "utf8");
  for (const [, key] of source.matchAll(/\bt\("(\w+)"\)/g)) {
    for (const language of languages) assert.ok(translate(language, key), `${language}.${key}`);
  }
  assert.throws(() => translate("en", "nonexistent"));
});
