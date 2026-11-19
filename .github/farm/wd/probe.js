document.getElementById("probe-input").addEventListener("input", (event) => {
  document.getElementById("echo").textContent = event.target.value;
});
