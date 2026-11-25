const rows = await rueca.listContracts({ side: "provided", query: "/api/tags" });
const ids = rows.filter((c) => c.key === "GET /api/tags" && c.framework !== "openapi").map((c) => c.path + "#" + c.local);
return [...new Set(ids)].sort();
