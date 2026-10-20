/**
 * Expected-hash input for Integrity Check ("Set expected hash").
 * Mirrors `normalize_hex_digest` and `digest_hex_len` in
 * packages/download-core/src/integrity.rs: whitespace is dropped, case is folded,
 * and the digest must be hex of the algorithm's length. The Core checks again;
 * this runs first so the user sees the problem before the request is sent.
 */

import type { HashAlgorithm } from "@/tauri/types";

/** Algorithm names accepted by `ExpectedHash::parse` (canonical spellings). */
export const EXPECTED_HASH_ALGORITHMS = ["sha-256", "sha-1", "md5"] as const;

export type ExpectedHashAlgorithm = (typeof EXPECTED_HASH_ALGORITHMS)[number];

/**
 * The Core serialises `HashAlgorithm` in kebab-case, so SHA-1 arrives over IPC as "sha1" while the
 * canonical spelling (and the string-table key) is "sha-1". Typed against the generated union so a
 * new Rust variant fails the typecheck here.
 */
export const IPC_HASH_ALGORITHM: Record<HashAlgorithm, ExpectedHashAlgorithm> = {
  md5: "md5",
  sha1: "sha-1",
  "sha-256": "sha-256",
};

const DIGEST_HEX_LENGTH: Record<ExpectedHashAlgorithm, number> = {
  "sha-256": 64,
  "sha-1": 40,
  md5: 32,
};

export function expectedDigestLength(algorithm: ExpectedHashAlgorithm): number {
  return DIGEST_HEX_LENGTH[algorithm];
}

export type ExpectedDigestResult =
  | { ok: true; digest: string }
  | {
      ok: false;
      reason: "empty" | "not_hex" | "wrong_length";
      expectedLength: number;
      actualLength: number;
    };

/** Normalize a user-typed digest and check it against the algorithm's length. */
export function normalizeExpectedDigest(
  algorithm: ExpectedHashAlgorithm,
  raw: string,
): ExpectedDigestResult {
  const expectedLength = expectedDigestLength(algorithm);
  const digest = raw.replace(/\s+/g, "").toLowerCase();
  if (digest.length === 0) {
    return { ok: false, reason: "empty", expectedLength, actualLength: 0 };
  }
  if (!/^[0-9a-f]+$/.test(digest)) {
    return { ok: false, reason: "not_hex", expectedLength, actualLength: digest.length };
  }
  if (digest.length !== expectedLength) {
    return { ok: false, reason: "wrong_length", expectedLength, actualLength: digest.length };
  }
  return { ok: true, digest };
}
