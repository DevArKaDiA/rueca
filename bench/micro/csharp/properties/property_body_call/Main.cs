namespace App;

public class Profile
{
    public string Display => Format();

    private string Format()
    {
        return "p";
    }

    public string Show()
    {
        return Display;
    }
}
