import { withBasePath } from "./base-path.js";

function currentReturnPath(location: Location = window.location): string {
  return `${location.pathname}${location.search}${location.hash}`;
}

function loginUrlForReturnPath(returnPath: string): string {
  return `${withBasePath("/login")}?return=${encodeURIComponent(returnPath)}`;
}

function redirectToLogin(location: Location = window.location): void {
  location.href = loginUrlForReturnPath(currentReturnPath(location));
}

export { currentReturnPath, loginUrlForReturnPath, redirectToLogin };
