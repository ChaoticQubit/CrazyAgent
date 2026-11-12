const BREADS = ["Sourdough", "Rye", "Baguette"];

function breadNames(breads) {
  return breads.map((name) => name.trim()).filter((name) => name.length > 0);
}

function showBreads(list, breads) {
  for (const name of breadNames(breads)) {
    const item = list.ownerDocument.createElement("li");
    item.textContent = name;
    list.append(item);
  }
}

if (typeof document !== "undefined") {
  showBreads(document.getElementById("breads"), BREADS);
}

if (typeof module !== "undefined") {
  module.exports = { BREADS, breadNames };
}
