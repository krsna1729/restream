let alertCount = 0;

function showErrorAlert(error: unknown): void {
  const errorAlertElem = document.getElementById("error-alert");
  const errorMsgElem = document.getElementById("error-msg");
  if (!errorAlertElem) return;
  errorAlertElem.classList.remove("hidden");
  if (errorMsgElem) errorMsgElem.innerText = String(error);
  console.error(error);
  const alertId = ++alertCount;
  setTimeout(() => {
    if (alertId !== alertCount) return;
    errorAlertElem.classList.add("hidden");
  }, 5000);
}

function showLoading(): void {
  document.getElementById("saving-badge")?.classList.add("flex");
  document.getElementById("saving-badge")?.classList.remove("hidden");
}

function hideLoading(): void {
  document.getElementById("saving-badge")?.classList.add("hidden");
  document.getElementById("saving-badge")?.classList.remove("flex");
}

export { showErrorAlert, showLoading, hideLoading };
