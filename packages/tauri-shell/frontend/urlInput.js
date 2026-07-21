/** URL input helpers backed by Download Core (#57). */

/**
 * @param {() => import("@tauri-apps/api/core").invoke} getInvoke
 */
export function createUrlInputApi(getInvoke) {
  return {
    async normalizeUrl(url) {
      return getInvoke()("normalize_url_cmd", { url });
    },

    async splitUrls(text) {
      return getInvoke()("split_urls_cmd", { text });
    },

    async findActiveJobForUrl(url) {
      return getInvoke()("find_active_job_for_url", { url });
    },
  };
}

/**
 * @typedef {"enqueue" | "focus" | "cancel"} DuplicateResolution
 */

/**
 * @param {object} opts
 * @param {(url: string) => Promise<object | null>} opts.findActiveJobForUrl
 * @param {(match: object) => Promise<DuplicateResolution>} opts.confirmDuplicate
 * @param {(match: object) => void | Promise<void>} opts.focusExisting
 */
export async function resolveDuplicateUrl({ findActiveJobForUrl, confirmDuplicate, focusExisting }, url) {
  const match = await findActiveJobForUrl(url);
  if (!match) {
    return "enqueue";
  }

  const resolution = await confirmDuplicate(match);
  if (resolution === "focus") {
    await focusExisting(match);
  }
  return resolution;
}

/**
 * @param {object} opts
 * @param {(text: string) => Promise<string[]>} opts.splitUrls
 * @param {(url: string) => Promise<DuplicateResolution>} opts.resolveDuplicate
 * @param {(url: string) => Promise<void>} opts.enqueueOne
 */
export async function processUrlInput({ splitUrls, resolveDuplicate, enqueueOne }, text) {
  const urls = await splitUrls(text);
  if (urls.length === 0) {
    return { enqueued: 0, total: 0 };
  }

  let enqueued = 0;
  for (const url of urls) {
    const resolution = await resolveDuplicate(url);
    if (resolution === "enqueue") {
      await enqueueOne(url);
      enqueued++;
    }
  }

  return { enqueued, total: urls.length };
}
