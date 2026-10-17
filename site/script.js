const copyButtons = document.querySelectorAll("[data-copy]");
const copyStatus = document.querySelector(".copy-status");

// A polite live region doesn't re-announce identical text, so clear it
// first and set the new message on the next frame.
const announce = (message) => {
  if (!copyStatus) {
    return;
  }
  copyStatus.textContent = "";
  window.requestAnimationFrame(() => {
    copyStatus.textContent = message;
  });
};

// Name the copy shortcut the reader's keyboard has, or none on a
// touch-only device.
const copyFailureMessage = (subject) => {
  const touchOnly =
    window.matchMedia("(pointer: coarse)").matches &&
    !window.matchMedia("(any-pointer: fine)").matches;
  if (touchOnly) {
    return `Couldn’t copy. Select the ${subject} and copy it.`;
  }
  const platform = navigator.userAgentData?.platform || navigator.platform || "";
  const isMac = /mac/i.test(platform);
  return isMac
    ? `Couldn’t copy. Select the ${subject} and press ⌘C.`
    : `Couldn’t copy. Select the ${subject} and press Ctrl+C.`;
};

copyButtons.forEach((copyButton) => {
  const title = copyButton.dataset.copyTitle ?? "Copy";
  let resetTimer;
  let errorMessage;

  const clearError = () => {
    copyButton.classList.remove("is-failed");
    errorMessage?.remove();
    errorMessage = undefined;
  };

  copyButton.addEventListener("click", async () => {
    const text = copyButton.dataset.copy ?? "";
    const label = copyButton.dataset.copyLabel ?? "Copied.";
    window.clearTimeout(resetTimer);

    try {
      await navigator.clipboard.writeText(text);
      clearError();
      announce(label);
      copyButton.classList.add("is-copied");
      copyButton.setAttribute("aria-label", label);
      copyButton.setAttribute("title", "Copied");
      resetTimer = window.setTimeout(() => {
        copyButton.classList.remove("is-copied");
        copyButton.setAttribute("aria-label", title);
        copyButton.setAttribute("title", title);
      }, 1800);
    } catch {
      // The clipboard API is blocked in some contexts. Say so where the
      // reader is looking, and name the manual way to copy.
      const failure = copyFailureMessage(copyButton.dataset.copySubject ?? "text");
      copyButton.classList.remove("is-copied");
      copyButton.setAttribute("aria-label", title);
      copyButton.setAttribute("title", title);
      copyButton.classList.add("is-failed");
      if (!errorMessage) {
        errorMessage = document.createElement("p");
        errorMessage.className = "copy-error";
        const anchor = copyButton.closest(".command, .code-panel") ?? copyButton;
        anchor.insertAdjacentElement("afterend", errorMessage);
      }
      errorMessage.textContent = failure;
      announce(failure);
    }
  });
});
