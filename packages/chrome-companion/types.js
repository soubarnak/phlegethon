/**
 * @typedef {Object} CompanionCookie
 * @property {string} name
 * @property {string} value
 * @property {string} domain
 * @property {string} path
 * @property {boolean} secure
 * @property {boolean} http_only
 * @property {number} expiration_date
 */

/**
 * @typedef {Object} CompanionSession
 * @property {CompanionCookie[]} cookies
 */

/**
 * @typedef {Object} CompanionRequest
 * @property {number} v
 * @property {string} op
 * @property {string} request_id
 * @property {string} page_url
 * @property {string | null} [media_url]
 * @property {string} [intent]
 * @property {string | null} [referrer]
 * @property {CompanionSession | null} [session]
 */

export {};
