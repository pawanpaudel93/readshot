const copyButtons = document.querySelectorAll("[data-copy]");
const copyStatus = document.querySelector(".copy-status");

copyButtons.forEach((copyButton) => {
  copyButton.addEventListener("click", async () => {
    const text = copyButton.dataset.copy ?? "";
    const label = copyButton.dataset.copyLabel ?? "Copied.";

    try {
      await navigator.clipboard.writeText(text);
      if (copyStatus) {
        copyStatus.textContent = label;
      }
      copyButton.classList.add("is-copied");
      copyButton.setAttribute("aria-label", label);
      copyButton.setAttribute("title", "Copied");
      window.setTimeout(() => {
        copyButton.classList.remove("is-copied");
        copyButton.setAttribute("aria-label", copyButton.dataset.copyTitle ?? "Copy");
        copyButton.setAttribute("title", copyButton.dataset.copyTitle ?? "Copy");
      }, 1800);
    } catch {
      if (copyStatus) {
        copyStatus.textContent = "Copy failed.";
      }
    }
  });
});
