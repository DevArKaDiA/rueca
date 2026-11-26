namespace App;

public class Settings
{
    public Settings()
    {
        Load();
    }

    public void Load()
    {
    }
}

public class Host
{
    public void Boot()
    {
        Settings settings = new();
    }
}
