namespace App;

public class Query
{
    public static Query From(string table)
    {
        return new Query();
    }

    public Query Where(string clause)
    {
        return this;
    }
}

public class Repo
{
    public Query Build()
    {
        return Query.From("t").Where("x");
    }
}
