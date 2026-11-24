const repos = await rueca.listRepositories();
const flask = repos.find((r) => r.repo === "github.com/pallets/flask");
return [...(flask.depends_on || [])].sort();
