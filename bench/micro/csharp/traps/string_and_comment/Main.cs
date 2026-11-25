namespace App;

public class Doc
{
    public void Save()
    {
    }

    public string Text()
    {
        // Save() is not called here
        return "Save()";
    }
}
