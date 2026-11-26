namespace App;

public static class TextExtensions
{
    public static string Slug(this string s)
    {
        return s.ToLower();
    }
}

public class Article
{
    public string Path(string title)
    {
        return TextExtensions.Slug(title);
    }
}
