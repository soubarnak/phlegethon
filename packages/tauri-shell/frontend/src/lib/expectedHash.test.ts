import { describe, expect, it } from "vitest";
import {
  EXPECTED_HASH_ALGORITHMS,
  expectedDigestLength,
  normalizeExpectedDigest,
} from "./expectedHash";

const HEX = "0123456789abcdef";
const digestOf = (length: number) => HEX.repeat(Math.ceil(length / HEX.length)).slice(0, length);

describe("expected digest length per algorithm", () => {
  it("matches the Core hex lengths", () => {
    expect(expectedDigestLength("md5")).toBe(32);
    expect(expectedDigestLength("sha-1")).toBe(40);
    expect(expectedDigestLength("sha-256")).toBe(64);
  });

  it("offers the three Core algorithms", () => {
    expect([...EXPECTED_HASH_ALGORITHMS].sort()).toEqual(["md5", "sha-1", "sha-256"]);
  });
});

describe("normalizeExpectedDigest", () => {
  it.each([
    ["md5", 32],
    ["sha-1", 40],
    ["sha-256", 64],
  ] as const)("accepts a correct %s digest", (algorithm, length) => {
    const digest = digestOf(length);
    expect(normalizeExpectedDigest(algorithm, digest)).toEqual({ ok: true, digest });
  });

  it.each([
    ["md5", 31],
    ["md5", 33],
    ["sha-1", 39],
    ["sha-1", 41],
    ["sha-256", 63],
    ["sha-256", 65],
  ] as const)("rejects an off-by-one %s digest of length %i", (algorithm, length) => {
    const result = normalizeExpectedDigest(algorithm, digestOf(length));
    expect(result).toEqual({
      ok: false,
      reason: "wrong_length",
      expectedLength: expectedDigestLength(algorithm),
      actualLength: length,
    });
  });

  it("rejects a sha-256 length passed as md5", () => {
    expect(normalizeExpectedDigest("md5", digestOf(64))).toMatchObject({
      ok: false,
      reason: "wrong_length",
      expectedLength: 32,
    });
  });

  it("rejects non-hex characters", () => {
    const bad = `g${digestOf(63).slice(1)}`;
    expect(normalizeExpectedDigest("sha-256", bad)).toMatchObject({ ok: false, reason: "not_hex" });
  });

  it("rejects a digest with an inner separator such as a colon", () => {
    const withColons = digestOf(32).replace(/(..)/g, "$1:").slice(0, -1);
    expect(normalizeExpectedDigest("md5", withColons)).toMatchObject({ ok: false, reason: "not_hex" });
  });

  it("reports empty input", () => {
    expect(normalizeExpectedDigest("sha-1", "")).toEqual({
      ok: false,
      reason: "empty",
      expectedLength: 40,
      actualLength: 0,
    });
    expect(normalizeExpectedDigest("sha-1", "   \n")).toMatchObject({ ok: false, reason: "empty" });
  });

  it("drops whitespace and folds case, as the Core does", () => {
    const result = normalizeExpectedDigest("md5", "ABCDEF0123456789 ABCDEF0123456789\n");
    expect(result).toEqual({ ok: true, digest: "abcdef0123456789abcdef0123456789" });
  });
});
