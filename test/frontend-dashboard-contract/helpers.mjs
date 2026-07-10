import {
  installFakeDom,
  loadCompiledFrontendModule,
} from "../helpers/fake-dom.mjs";

function appendRoot(document, tagName, id) {
  const element = document.createElement(tagName);
  element.id = id;
  document.body.appendChild(element);
  return element;
}

async function flushAsyncWork() {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

async function waitForCondition(check, attempts = 40) {
  for (let i = 0; i < attempts; i += 1) {
    if (check()) return;
    await Promise.resolve();
  }
}

export {
  appendRoot,
  flushAsyncWork,
  installFakeDom,
  loadCompiledFrontendModule,
  waitForCondition,
};
