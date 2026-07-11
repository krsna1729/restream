(() => {
  const form = document.getElementById("login-form");
  const input = document.getElementById("password-input");
  const toggleBtn = document.getElementById("toggle-password-btn");
  const eyeIcon = document.getElementById("eye-icon");
  const eyeOffIcon = document.getElementById("eye-off-icon");
  const submitBtn = document.getElementById("login-btn");
  const errorEl = document.getElementById("login-error");

  const basePath = String(window.__RESTREAM_BASE_PATH__ || "").replace(
    /\/+$/,
    "",
  );
  const withBasePath = (path) => `${basePath}${path}`;

  toggleBtn?.addEventListener("click", () => {
    if (!(input instanceof HTMLInputElement)) return;
    const show = input.type === "password";
    input.type = show ? "text" : "password";
    eyeIcon?.classList.toggle("hidden", show);
    eyeOffIcon?.classList.toggle("hidden", !show);
    toggleBtn.setAttribute("aria-pressed", show ? "true" : "false");
    toggleBtn.setAttribute("aria-label", show ? "Hide password" : "Show password");
  });

  form?.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (
      !(input instanceof HTMLInputElement) ||
      !(submitBtn instanceof HTMLButtonElement) ||
      !(errorEl instanceof HTMLElement)
    ) {
      return;
    }

    submitBtn.disabled = true;
    errorEl.classList.add("hidden");

    try {
      const res = await fetch(withBasePath("/api/v1/auth/login"), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ password: input.value }),
      });
      if (res.ok) {
        window.location.href = withBasePath("/");
        return;
      }
      const data = await res.json();
      errorEl.textContent = data.error || "Login failed";
      errorEl.classList.remove("hidden");
    } catch {
      errorEl.textContent = "Request failed";
      errorEl.classList.remove("hidden");
    } finally {
      submitBtn.disabled = false;
    }
  });
})();
