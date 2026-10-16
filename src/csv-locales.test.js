import { test } from "node:test";
import assert from "node:assert/strict";
import { csvMessages, csvText } from "./csv-locales.js";

test("CSV interface translations have matching keys and nonempty values", () => {
  const keys = Object.keys(csvMessages.en).sort();
  for (const language of ["en", "es", "fr"]) {
    assert.deepEqual(Object.keys(csvMessages[language]).sort(), keys);
    for (const key of keys) {
      assert.ok(csvMessages[language][key].trim(), `${language}.${key}`);
      const placeholders = (value) => [...value.matchAll(/\{(\w+)\}/g)].map((match) => match[1]).sort();
      assert.deepEqual(placeholders(csvMessages[language][key]), placeholders(csvMessages.en[key]));
    }
  }
});

test("CSV summaries and error locations interpolate without changing input data", () => {
  for (const language of ["en", "es", "fr"]) {
    assert.ok(csvText(language, "imported", { count: 42 }).includes("42"));
    assert.ok(csvText(language, "location", { record: 5, column: "publication_year" }).includes("publication_year"));
    assert.ok(!csvText(language, "count", { count: 2 }).includes("{"));
  }
});
