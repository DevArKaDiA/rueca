PRAGMA foreign_keys = ON;

CREATE TABLE [Artist] (
  [ArtistId] INTEGER NOT NULL PRIMARY KEY,
  [Name] NVARCHAR(120)
);

CREATE TABLE [Album] (
  [AlbumId] INTEGER NOT NULL PRIMARY KEY,
  [ArtistId] INTEGER NOT NULL,
  FOREIGN KEY ([ArtistId]) REFERENCES [Artist] ([ArtistId])
);

CREATE VIEW album_artist AS
SELECT al.AlbumId, ar.Name FROM Album al JOIN Artist ar ON ar.ArtistId = al.ArtistId;
