(() => {
  const control = /[\u0000-\u0008\u000b\u000c\u000e-\u001f]/u;
  const controls = /[\u0000-\u0008\u000b\u000c\u000e-\u001f]/gu;
  const typed = (target) => target instanceof HTMLTextAreaElement
    || (target instanceof HTMLInputElement && ["", "text", "search", "url", "tel", "password"].includes(target.type));
  addEventListener("beforeinput", (event) => {
    if (!event.inputType.startsWith("insert") || !event.data || event.data.replace(controls, "") !== "") return;
    if (!event.cancelable) return;
    event.preventDefault();
    event.stopImmediatePropagation();
  }, true);
  addEventListener("input", (event) => {
    const target = event.target;
    if (!typed(target) || !control.test(target.value)) return;
    const caret = target.value.slice(0, target.selectionStart ?? target.value.length).replace(controls, "").length;
    target.value = target.value.replace(controls, "");
    target.setSelectionRange?.(caret, caret);
  }, true);
})();
