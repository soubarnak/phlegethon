const FOCUSABLE =
  'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * @param {ParentNode} root
 * @returns {HTMLElement[]}
 */
export function getFocusableElements(root) {
  return [...root.querySelectorAll(FOCUSABLE)].filter(
    (el) => el && !el.disabled && el.offsetParent !== null,
  );
}

/**
 * Trap Tab within a container. Returns a cleanup function.
 * @param {HTMLElement} container
 * @param {{ onEscape?: () => void }} [options]
 * @returns {() => void}
 */
export function trapFocus(container, options = {}) {
  function onKeyDown(event) {
    if (event.key === "Escape") {
      options.onEscape?.();
      return;
    }
    if (event.key !== "Tab") {
      return;
    }

    const focusable = getFocusableElements(container);
    if (focusable.length === 0) {
      return;
    }

    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement;

    if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  container.addEventListener("keydown", onKeyDown);
  return () => container.removeEventListener("keydown", onKeyDown);
}

/**
 * @param {HTMLElement} container
 * @returns {HTMLElement | undefined}
 */
export function focusFirst(container) {
  const [first] = getFocusableElements(container);
  first?.focus();
  return first;
}
