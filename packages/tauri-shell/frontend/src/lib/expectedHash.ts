/**
 * Expected-hash input for Integrity Check ("Set expected hash").
 * Mirrors `normalize_hex_digest` and `digest_hex_len` in
 * packages/download-core/src/integrity.rs: whitespace is dropped, case is folded,
 * and the digest must be hex of the algorithm's length. The Core checks again;
 * this runs first so the user sees the problem before the request is sent.
 */

/** Algorithm names accepted by `ExpectedHash::parse` (canonical spellings). */
export const EXPECTED_HASH_ALGORITHMS = ["sha-256", "sha-1", "md5"] as const;

export type ExpectedHashAlgorithm = (typeof EXPECTED_HASH_ALGORITHMS)[number];

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
